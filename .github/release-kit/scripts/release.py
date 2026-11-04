"""The buchochelliq-labs release driver: release-kit.yml, checked, built, tagged and gated.

release-kit.yml lists every package a repository publishes, on which registry,
from which manifest, at which version, in publication order, and the
artifacts each one is built into. A release is cut from one commit of the
default branch and named by one package's version (the last, unless
`release: name_from` says otherwise). Generalised from rs-rich-cli's
release_cohort.py; the rules are the same:

  - A published version is permanent: nothing already on its registry is
    tagged, selected or uploaded again.
  - Every tag is annotated and records its release and commit, and points at a
    commit on the default branch.
  - A release is one commit: a package already tagged on another commit that
    ships other files under the same version is refused (a "split" release).
  - A failed release is finished by running `tag` again: a run that provably
    published nothing has its tags moved or pushed again; a run that published
    part of the release is pushed again and publishes only what is still due.

Commands (person):
    check       release-kit.yml matches every manifest, names no package twice,
                lists each package after the listed packages it depends on,
                and declares artifacts each registry can publish.
    sync        rewrite release-kit.yml's versions from the manifests.
    status      each package: version, tag, and whether it is published.
    tag         tag the release and wait for it: an annotated record tag per
                package due (which starts nothing), then the release tag,
                which starts the one release run. --commit, --release, --dry-run.

Commands (the gate action, in CI):
    gate TAG    the tag is annotated, on the default branch, records its
                commit, does not split its release, and something is due.
                Writes sha, release, selection and per-registry flags to
                GITHUB_OUTPUT.
    preflight --registry R --selection JSON
                refuse, before uploading, any selected package of R already on R.
    verify --registry R --selection JSON
                wait until every selected package of R is on R.
    plan --selection JSON
                the build matrix: one job per artifact (and target) of the
                selected packages. Writes `jobs` and `count` to GITHUB_OUTPUT.
    prepare --job JSON
                run the job's artifact's `prepare` command, before building.
    build --job JSON
                build one planned job into dist/, with a .sha256 per file for
                GitHub Releases.
    github-release --name NAME --stage DIR --notes-out FILE
                shape a github package's release as release-kit.yml says:
                write its `checksums` file into DIR, its `notes` into FILE,
                and print what the publish action should do (JSON).

    render --name NAME --channel homebrew|chocolatey --sums FILE --out DIR
                render a github package's Homebrew formula or Chocolatey
                package from its templates and its release's checksums.
    homebrew-tap --tap DIR --formula FILE --repo OWNER/TAP --version V --release-url URL
                put the formula on a branch of the tap and open a pull request.

Commands (installing):
    install     from a release-kit checkout, copy the kit into the repository
                you are in: .github/release-kit/ (this script and the actions)
                and .github/workflows/release-kit-build.yml, with a manifest of
                checksums; and a starter release.yml if it has none.
    vendored    the installed copy is unmodified (run it in CI): it changes
                only by installing again.

Registries: github (GitHub Releases), crates.io, pypi, npm, vscode (Visual
Studio Marketplace). Artifact kinds: rust-binary, python, maturin, vsix, ci-artifact and
command. Requires Python 3.11+, git and PyYAML (for release-kit.yml); `tag`
follows runs with the GitHub CLI (`gh`) when it is installed.
"""

import sys

if sys.version_info < (3, 11):
    sys.exit(f"release.py needs Python 3.11 or newer (for tomllib); this is {sys.version.split()[0]}")

import argparse
from dataclasses import dataclass, field
import fnmatch
import hashlib
import io
import json
import os
import posixpath
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tarfile
import tempfile
import time
import tomllib
import zipfile
from urllib.error import HTTPError, URLError
from urllib.parse import quote
from urllib.request import HTTPRedirectHandler, Request, build_opener, urlopen

CONFIG = "release-kit.yml"
REGISTRIES = ("github", "crates.io", "pypi", "npm", "vscode")
NUMBER = r"(?:0|[1-9][0-9]*)"
PRERELEASE = rf"(?:{NUMBER}|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
SEMVER = rf"{NUMBER}\.{NUMBER}\.{NUMBER}(?:-{PRERELEASE}(?:\.{PRERELEASE})*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
# PyPI versions follow PEP 440 (1.2.0rc1, 1.2.0.post1), not SemVer.
PEP440 = r"[0-9]+(?:\.[0-9]+)*(?:(?:a|b|rc)[0-9]+)?(?:\.post[0-9]+)?(?:\.dev[0-9]+)?"
USER_AGENT = "buchochelliq-labs-release-kit (https://github.com/buchochelliq-labs/release-kit)"


def root():
    return Path(subprocess.check_output(["git", "rev-parse", "--show-toplevel"], text=True).strip())


def git(*args, capture=True, check=True):
    result = subprocess.run(["git", *args], cwd=root(), text=True, encoding="utf-8",
                            capture_output=capture, check=check)
    return result.stdout.strip() if capture else ""


def files_on_disk(base=None):
    base = base or root()
    return lambda path: (base / path).read_text(encoding="utf-8")


def files_at(commit):
    return lambda path: git("show", f"{commit}:{path}")


# ---------------------------------------------------------------------------
# release-kit.yml


@dataclass(frozen=True)
class Package:
    name: str
    registry: str
    manifest: str
    version: str
    tag_format: str = "{name}-v{version}"
    paths: tuple = ()
    artifacts: tuple = field(default=(), compare=False, hash=False)
    # `github` packages only: how the GitHub release is made (GITHUB_OPTIONS).
    github: tuple = field(default=(), compare=False, hash=False)

    @property
    def slug(self):
        """The name as a file and artifact name: @org/pkg -> org-pkg."""
        return re.sub(r"[^A-Za-z0-9._-]+", "-", self.name).strip("-")

    @property
    def tag(self):
        return self.tag_format.format(name=self.name, version=self.version)

    @property
    def shipped(self):
        """The files whose change means a different package: `paths`, else
        the manifest's directory."""
        if self.paths:
            return self.paths
        parent = str(PurePosixPath(self.manifest).parent)
        return ("**",) if parent == "." else (f"{parent}/**",)

    def __str__(self):
        return f"{self.name} {self.version} ({self.registry})"


@dataclass(frozen=True)
class Release:
    packages: tuple
    name_from: str
    tag_format: str
    branch: str

    @property
    def name(self):
        return next(p.version for p in self.packages if p.name == self.name_from)

    @property
    def tag(self):
        return self.tag_format.format(release=self.name)


def parse_config(text):
    try:
        import yaml
    except ImportError:
        raise RuntimeError(f"release.py reads {CONFIG} with PyYAML: pip install pyyaml") from None
    data = yaml.safe_load(text) or {}
    if not isinstance(data, dict):
        raise ValueError(f"{CONFIG} must be a mapping with `packages:`")
    return data


def load(read):
    data = parse_config(read(CONFIG))
    entries = data.get("packages") or []
    if not entries:
        raise ValueError(f"{CONFIG} lists no packages")
    packages = []
    for entry in entries:
        missing = {"name", "registry", "manifest", "version"} - entry.keys()
        if missing:
            raise ValueError(f"{CONFIG}: a package lacks {', '.join(sorted(missing))}")
        if not isinstance(entry["version"], str):
            # YAML reads 0.10 as the number 0.1: versions must be quoted.
            raise ValueError(f"{entry['name']}: quote the version (version: \"{entry['version']}\"); "
                             "YAML reads an unquoted one as a number")
        artifacts = entry.get("artifacts") or []
        if not isinstance(artifacts, list) or not all(isinstance(a, dict) for a in artifacts):
            raise ValueError(f"{entry['name']}: artifacts must be a list of mappings")
        options = {key: entry[key] for key in GITHUB_OPTIONS if key in entry}
        packages.append(Package(entry["name"], entry["registry"], entry["manifest"], entry["version"],
                                entry.get("tag", "{name}-v{version}"), tuple(entry.get("paths") or ()),
                                tuple(artifacts), tuple(sorted(options.items()))))
    settings = data.get("release") or {}
    name_from = settings.get("name_from", packages[-1].name)
    if name_from not in {p.name for p in packages}:
        raise ValueError(f"{CONFIG}: release.name_from = {name_from!r} is not a listed package")
    return Release(tuple(packages), name_from, settings.get("tag", "release-{release}"),
                   settings.get("branch", "main"))


# The keys a `github` package takes to shape its release: `notes`, a command
# whose output is the release's notes (else GitHub generates them);
# `checksums`, the name of one file listing every asset's SHA-256; `attest`,
# whether to attest the assets' build provenance. And the channels made from
# the release once it is out: `homebrew` (a formula, and a pull request on a
# tap) and `chocolatey` (a package pushed to the community repository).
GITHUB_OPTIONS = ("notes", "checksums", "attest", "homebrew", "chocolatey")
CHANNEL_KEYS = {
    "homebrew": ({"tap", "formula"}, {"name"}),
    "chocolatey": ({"package"}, {"test"}),
}


def github_option_errors(package):
    """What is wrong with a package's GitHub release options, as messages."""
    options = dict(package.github)
    if options and package.registry != "github":
        return [f"{package.name}: {', '.join(sorted(options))} only apply to a github package"]
    errors = []
    if "notes" in options and not (isinstance(options["notes"], str) and options["notes"].strip()):
        errors.append(f"{package.name}: notes is a command that prints the release notes")
    if "checksums" in options:
        name = options["checksums"]
        if not (isinstance(name, str) and re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9._-]*", name)):
            errors.append(f"{package.name}: checksums is a file name, e.g. SHA256SUMS")
    if "attest" in options and not isinstance(options["attest"], bool):
        errors.append(f"{package.name}: attest is true or false")
    for channel, (required, optional) in CHANNEL_KEYS.items():
        if channel not in options:
            continue
        settings = options[channel]
        if not isinstance(settings, dict):
            errors.append(f"{package.name}: {channel} is a mapping ({', '.join(sorted(required))})")
            continue
        if missing := required - settings.keys():
            errors.append(f"{package.name}: {channel} needs {', '.join(sorted(missing))}")
        if unknown := settings.keys() - required - optional:
            errors.append(f"{package.name}: {channel} does not take {', '.join(sorted(unknown))}")
        if not all(isinstance(v, str) and v.strip() for v in settings.values()):
            errors.append(f"{package.name}: {channel}'s settings are text")
        tap = settings.get("tap") if channel == "homebrew" else None
        if isinstance(tap, str) and not re.fullmatch(r"[\w.-]+/[\w.-]+", tap):
            errors.append(f"{package.name}: homebrew tap is owner/repository, e.g. org/homebrew-tap")
    return errors


def version_key(version):
    """Numeric parts first, so 0.0.10 sorts after 0.0.9."""
    return tuple(int(part) for part in re.findall(r"\d+", version.split("+")[0])[:3])


# Manifests: how each registry's manifest names its package and version.

def _cargo(read, path):
    data = tomllib.loads(read(path))
    package = data.get("package", {})
    version = package.get("version")
    if isinstance(version, dict) and version.get("workspace"):
        workspace = tomllib.loads(read("Cargo.toml"))
        version = workspace["workspace"]["package"]["version"]
    deps = set()
    tables = [data.get("dependencies", {}), data.get("build-dependencies", {})]
    tables += [t.get("dependencies", {}) for t in data.get("target", {}).values()]
    for table in tables:
        for key, spec in table.items():
            deps.add(spec.get("package", key) if isinstance(spec, dict) else key)
    return package.get("name"), version, deps


def maturin_manifest(read, path):
    """The Cargo.toml maturin builds a pyproject.toml from: `[tool.maturin]
    manifest-path` (relative to the pyproject's directory), else the
    Cargo.toml beside it. Returns (path from the repository root, whether
    pyproject.toml names it)."""
    data = tomllib.loads(read(path))
    parent = PurePosixPath(path).parent
    named = data.get("tool", {}).get("maturin", {}).get("manifest-path")
    if named:
        return str(PurePosixPath(posixpath.normpath(str(parent / named)))), True
    return str(parent / "Cargo.toml"), False


def pep440(version):
    """A Cargo version as maturin writes it into a wheel: 1.0.0-rc.1 -> 1.0.0rc1."""
    core, _, pre = version.partition("-")
    if not pre:
        return version
    match = re.fullmatch(r"(alpha|beta|rc|a|b|c)\.?(\d+)", pre)
    if not match:
        return version
    label = {"alpha": "a", "beta": "b", "c": "rc"}.get(match[1], match[1])
    return f"{core}{label}{match[2]}"


def _pyproject(read, path):
    data = tomllib.loads(read(path))
    project = data.get("project") or data.get("tool", {}).get("poetry", {})
    deps = {re.split(r"[\s<>=!~;\[(]", d, maxsplit=1)[0] for d in project.get("dependencies", [])
            if isinstance(d, str)}
    version = project.get("version")
    if version is None and "version" in project.get("dynamic", []) and "maturin" in data.get("tool", {}):
        # maturin takes a dynamic version from the crate it builds.
        cargo_version = _cargo(read, maturin_manifest(read, path)[0])[1]
        version = pep440(cargo_version) if cargo_version else None
    return project.get("name"), version, deps


def _package_json(read, path, vscode=False):
    data = json.loads(read(path))
    name = f"{data.get('publisher')}.{data.get('name')}" if vscode else data.get("name")
    deps = set(data.get("dependencies", {})) | set(data.get("peerDependencies", {}))
    return name, data.get("version"), deps


def manifest_info(read, package):
    """(name, version, dependency names) from the package's manifest."""
    file = PurePosixPath(package.manifest).name
    if file == "Cargo.toml":
        return _cargo(read, package.manifest)
    if file == "pyproject.toml":
        return _pyproject(read, package.manifest)
    if file == "package.json":
        return _package_json(read, package.manifest, vscode=package.registry == "vscode")
    raise ValueError(f"{package.name}: unsupported manifest {package.manifest} "
                     "(use Cargo.toml, pyproject.toml or package.json)")


def check(read):
    """Every way release-kit.yml can disagree with the repository, as messages."""
    try:
        release = load(read)
    except (ValueError, tomllib.TOMLDecodeError, KeyError) as error:
        return [str(error)]
    errors, seen, tags, deps = [], set(), {}, {}
    for package in release.packages:
        if package.registry not in REGISTRIES:
            errors.append(f"{package.name}: unknown registry {package.registry!r} (use {', '.join(REGISTRIES)})")
            continue
        if (package.registry, package.name) in seen:
            errors.append(f"{package}: listed twice")
        seen.add((package.registry, package.name))
        grammar = PEP440 if package.registry == "pypi" else SEMVER
        if not re.fullmatch(grammar, package.version):
            errors.append(f"{package.name}: {package.version!r} is not a version")
        if package.tag in tags:
            errors.append(f"{package.name} and {tags[package.tag]} have the same tag {package.tag}")
        tags[package.tag] = package.name
        if package.tag == release.tag:
            errors.append(f"{package.name}'s tag {package.tag} is the release tag; record tags must differ")
        try:
            name, version, deps[package.name] = manifest_info(read, package)
        except (subprocess.CalledProcessError, FileNotFoundError, KeyError, ValueError,
                tomllib.TOMLDecodeError, json.JSONDecodeError) as error:
            errors.append(f"{package.name}: cannot read {package.manifest} ({error})")
            continue
        if version != package.version:
            errors.append(f"{package.name}: {CONFIG} says {package.version}, {package.manifest} says {version} "
                          "(run release.py sync)")
        if package.registry != "github" and name != package.name:
            errors.append(f"{package.name}: {package.manifest} names the package {name!r}")
    for package in release.packages:
        errors.extend(artifact_errors(package))
        errors.extend(github_option_errors(package))
    # Each package after the listed packages of its own registry it depends on.
    position = {(p.registry, p.name): i for i, p in enumerate(release.packages)}
    for package in release.packages:
        for dependency in sorted(deps.get(package.name, ())):
            index = position.get((package.registry, dependency))
            if index is not None and index > position[(package.registry, package.name)]:
                errors.append(f"{package.name} is listed before {dependency}, which it depends on")
    return errors


def sync(read=None, path=None):
    """Rewrite each version in release-kit.yml from its manifest, keeping the
    file's comments and layout. Returns the changes made."""
    path = path or root() / CONFIG
    read = read or files_on_disk(path.parent)
    text = path.read_text(encoding="utf-8")
    release = load(lambda name: text if name == CONFIG else read(name))
    # One `version:` line per package, in order (artifacts carry none).
    lines = text.splitlines(keepends=True)
    found = [i for i, line in enumerate(lines) if re.match(r"^\s*(?:-\s+)?version:", line)]
    if len(found) != len(release.packages):
        raise ValueError(f"{CONFIG}: expected one `version:` line per package, found {len(found)}")
    changes = []
    for index, package in zip(found, release.packages):
        version = manifest_info(read, package)[1]
        if version != package.version:
            lines[index] = re.sub(r"(version:\s*)([\"']?)[^\"'\s#]+([\"']?)",
                                  lambda m: f'{m[1]}"{version}"', lines[index], count=1)
            changes.append(f"{package.name}: {package.version} -> {version}")
    path.write_text("".join(lines), encoding="utf-8")
    return changes


# ---------------------------------------------------------------------------
# Registries


def http_status(url, data=None, headers=None):
    request = Request(url, data=data, headers={"User-Agent": USER_AGENT, **(headers or {})})
    for attempt in range(4):
        try:
            with urlopen(request, timeout=30) as response:
                return response.status, response.read()
        except HTTPError as error:
            if error.code == 404:
                return 404, b""
            if error.code < 500 and error.code != 429:
                raise
        except URLError:
            pass
        time.sleep(2 ** attempt)
    raise RuntimeError(f"{url}: the registry did not answer; try again")


def repository():
    """owner/name, from GITHUB_REPOSITORY in CI, else the origin remote."""
    if slug := os.environ.get("GITHUB_REPOSITORY"):
        return slug
    url = git("remote", "get-url", "origin")
    match = re.search(r"github\.com[:/]([^/]+/[^/]+?)(?:\.git)?$", url)
    if not match:
        raise RuntimeError(f"cannot tell the GitHub repository from {url}")
    return match[1]


def published_on(package):
    """Whether `package`'s exact version is on its registry."""
    name, version = package.name, package.version
    if package.registry == "crates.io":
        return http_status(f"https://crates.io/api/v1/crates/{quote(name)}/{quote(version)}")[0] == 200
    if package.registry == "pypi":
        return http_status(f"https://pypi.org/pypi/{quote(name)}/{quote(version)}/json")[0] == 200
    if package.registry == "npm":
        return http_status(f"https://registry.npmjs.org/{quote(name, safe='@')}/{quote(version)}")[0] == 200
    if package.registry == "vscode":
        query = {"filters": [{"criteria": [{"filterType": 7, "value": name}]}], "flags": 0x1}
        status, body = http_status(
            "https://marketplace.visualstudio.com/_apis/public/gallery/extensionquery",
            data=json.dumps(query).encode(),
            headers={"Content-Type": "application/json", "Accept": "application/json;api-version=7.2-preview.1"})
        results = json.loads(body or b"{}").get("results", [{}])[0].get("extensions", [])
        return any(v.get("version") == version for e in results for v in e.get("versions", []))
    if package.registry == "github":
        token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
        headers = {"Accept": "application/vnd.github+json", **({"Authorization": f"Bearer {token}"} if token else {})}
        status, body = http_status(f"https://api.github.com/repos/{repository()}/releases/tags/{quote(package.tag)}",
                                   headers=headers)
        # The release is created as a draft and published once every file is up
        # and checked: a draft (which this endpoint does not serve) is unfinished.
        release = json.loads(body) if status == 200 else {}
        return bool(release.get("assets")) and not release.get("draft")
    raise ValueError(f"unknown registry {package.registry!r}")


# ---------------------------------------------------------------------------
# Artifacts: what each package is built into, declared in release-kit.yml

# Rust targets and the GitHub runner that builds each. Intel macOS is
# cross-compiled on Apple Silicon (GitHub retired the Intel runners).
RUNNERS = {
    "x86_64-unknown-linux-gnu": "ubuntu-latest",
    "aarch64-unknown-linux-gnu": "ubuntu-24.04-arm",
    # Static Linux binaries; each is built natively (the build action installs
    # musl-tools for a rust-binary, maturin-action builds wheels in its containers).
    "x86_64-unknown-linux-musl": "ubuntu-latest",
    "aarch64-unknown-linux-musl": "ubuntu-24.04-arm",
    "aarch64-apple-darwin": "macos-latest",
    "x86_64-apple-darwin": "macos-latest",
    "x86_64-pc-windows-msvc": "windows-latest",
    "aarch64-pc-windows-msvc": "windows-11-arm",
}
DEFAULT_TARGETS = ["x86_64-unknown-linux-gnu", "aarch64-unknown-linux-gnu", "aarch64-apple-darwin",
                   "x86_64-apple-darwin", "x86_64-pc-windows-msvc"]
# kind: (registries it can feed, required fields, optional fields)
KINDS = {
    "rust-binary": ({"github"}, {"bin"}, {"crate", "targets", "include", "test", "prepare"}),
    "python": ({"pypi", "github"}, set(), {"dir", "test", "prepare"}),
    "maturin": ({"pypi", "github"}, set(), {"targets", "sdist", "test", "prepare"}),
    "vsix": ({"vscode", "github"}, set(), {"dir", "test", "prepare"}),
    "command": ({"github", "pypi", "vscode"}, {"run", "outputs"}, {"os", "test", "prepare"}),
    # Promote the exact files a CI run already built and tested, instead of
    # building them again: the artifact id and its SHA-256 are pinned.
    "ci-artifact": ({"github", "pypi", "vscode"}, {"artifact", "sha256", "files"}, {"test"}),
}
# Registries that publish from source need no artifacts; the others do.
FROM_SOURCE = {"crates.io", "npm"}


def targets_of(artifact):
    """[(target, runner)], from strings or {target, runner} mappings."""
    out = []
    for entry in artifact.get("targets") or DEFAULT_TARGETS:
        if isinstance(entry, dict):
            target = entry.get("target")
            out.append((target, entry.get("runner") or RUNNERS.get(target)))
        else:
            out.append((entry, RUNNERS.get(entry)))
    return out


def test_of(artifact, target=""):
    """The artifact's `test` command for a build job, or None: a string runs
    after every job of the artifact; {run, targets} only after the listed
    targets' jobs (a cross-compiled wheel cannot be installed on its runner)."""
    test = artifact.get("test")
    if isinstance(test, str):
        return test
    if isinstance(test, dict) and (not test.get("targets") or target in test["targets"]):
        return test.get("run")
    return None


def artifact_errors(package):
    """What is wrong with a package's artifacts, as messages."""
    errors = []
    if package.registry in FROM_SOURCE:
        if package.artifacts:
            errors.append(f"{package.name}: {package.registry} publishes from source; remove its artifacts")
        return errors
    if not package.artifacts:
        errors.append(f"{package.name}: {package.registry} needs at least one artifact "
                      f"({', '.join(k for k, v in KINDS.items() if package.registry in v[0])})")
    for index, artifact in enumerate(package.artifacts):
        kind = artifact.get("kind")
        where = f"{package.name}: artifact {index + 1}"
        if kind not in KINDS:
            errors.append(f"{where}: unknown kind {kind!r} (use {', '.join(KINDS)})")
            continue
        registries, required, optional = KINDS[kind]
        if package.registry not in registries:
            errors.append(f"{where}: {kind} cannot be published to {package.registry}")
        if missing := required - artifact.keys():
            errors.append(f"{where}: {kind} needs {', '.join(sorted(missing))}")
        if unknown := artifact.keys() - required - optional - {"kind"}:
            errors.append(f"{where}: {kind} does not take {', '.join(sorted(unknown))}")
        test = artifact.get("test")
        if test is not None and not (isinstance(test, str) or (
                isinstance(test, dict) and isinstance(test.get("run"), str)
                and set(test) <= {"run", "targets"})):
            errors.append(f"{where}: test is a command, or {{run: ..., targets: [...]}}")
        elif isinstance(test, dict) and test.get("targets") and kind not in ("rust-binary", "maturin"):
            errors.append(f"{where}: {kind} is not built per target; give test as a command")
        if "prepare" in artifact and not (isinstance(artifact["prepare"], str) and artifact["prepare"].strip()):
            errors.append(f"{where}: prepare is a command")
        if kind == "ci-artifact":
            if not (isinstance(artifact.get("artifact"), int) and not isinstance(artifact.get("artifact"), bool)
                    and artifact["artifact"] > 0):
                errors.append(f"{where}: artifact is the CI artifact's numeric id")
            if not (isinstance(artifact.get("sha256"), str) and re.fullmatch(r"[0-9a-f]{64}", artifact["sha256"])):
                errors.append(f"{where}: sha256 is the artifact archive's SHA-256, 64 lowercase hex digits")
            if not (isinstance(artifact.get("files"), str) and artifact["files"].strip()):
                errors.append(f"{where}: files is a glob of the archive members to publish")
        if kind in ("rust-binary", "maturin"):
            for target, runner in targets_of(artifact):
                if not runner:
                    errors.append(f"{where}: no runner known for target {target!r}; "
                                  "give it as {target: ..., runner: ...}")
    return errors


def job_id(*parts):
    return re.sub(r"[^A-Za-z0-9._-]+", "-", "-".join(str(p) for p in parts if p)).strip("-")


def plan(selection, read=None, output=None):
    """The build matrix for the selected packages: one job per artifact, or per
    artifact and target. Writes `jobs` (JSON) and `count` to GITHUB_OUTPUT."""
    release = load(read or files_on_disk())
    wanted = {(s["registry"], s["name"]) for s in json.loads(selection)}
    jobs = []
    for package in release.packages:
        if (package.registry, package.name) not in wanted:
            continue
        base = {"package": package.name, "slug": package.slug, "version": package.version,
                "registry": package.registry, "manifest": package.manifest}
        for index, artifact in enumerate(package.artifacts):
            kind = artifact["kind"]
            base["prepare"] = artifact.get("prepare") or ""
            if kind == "rust-binary":
                crate = artifact.get("crate") or manifest_info(read or files_on_disk(), package)[0]
                for target, runner in targets_of(artifact):
                    jobs.append({**base, "id": job_id(package.slug, kind, target), "kind": kind, "os": runner,
                                 "target": target, "bin": artifact["bin"], "crate": crate,
                                 "include": list(artifact.get("include") or []),
                                 "test": test_of(artifact, target)})
            elif kind == "maturin":
                # maturin builds from the crate beside pyproject.toml, or the one its
                # [tool.maturin] manifest-path names: then maturin runs in the
                # pyproject's directory and finds it itself (`cargo` is empty).
                cargo, named = maturin_manifest(read or files_on_disk(), package.manifest)
                directory = str(PurePosixPath(package.manifest).parent)
                where = {"cargo": "", "dir": directory} if named else {"cargo": cargo, "dir": "."}
                for target, runner in targets_of(artifact):
                    jobs.append({**base, "id": job_id(package.slug, kind, target), "kind": kind, "os": runner,
                                 "target": target, **where, "test": test_of(artifact, target)})
                if artifact.get("sdist", True):
                    jobs.append({**base, "id": job_id(package.slug, "sdist"), "kind": "maturin-sdist",
                                 "os": "ubuntu-latest", "target": "", **where, "test": None})
            elif kind in ("python", "vsix"):
                directory = artifact.get("dir") or str(PurePosixPath(package.manifest).parent)
                jobs.append({**base, "id": job_id(package.slug, kind, index), "kind": kind,
                             "os": "ubuntu-latest", "target": "", "dir": directory, "test": test_of(artifact)})
            elif kind == "command":
                jobs.append({**base, "id": job_id(package.slug, kind, index), "kind": kind,
                             "os": artifact.get("os", "ubuntu-latest"), "target": "",
                             "run": artifact["run"], "outputs": artifact["outputs"], "test": test_of(artifact)})
            elif kind == "ci-artifact":
                jobs.append({**base, "id": job_id(package.slug, kind, index), "kind": kind,
                             "os": "ubuntu-latest", "target": "", "artifact": artifact["artifact"],
                             "sha256": artifact["sha256"], "files": artifact["files"], "test": test_of(artifact)})
    ids = [j["id"] for j in jobs]
    if len(ids) != len(set(ids)):
        raise ValueError("two artifacts plan the same build job; give them different targets")
    outputs = {"jobs": json.dumps(jobs), "count": str(len(jobs))}
    target = output or os.environ.get("GITHUB_OUTPUT")
    if target:
        with open(target, "a", encoding="utf-8") as handle:
            for key, value in outputs.items():
                handle.write(f"{key}={value}\n")
    for job in jobs:
        print(f"build  {job['id']} on {job['os']}", flush=True)
    return jobs


def sha256_file(path):
    """Write `<file>.sha256` as `sha256sum` does: '<hex>  <name>'."""
    digest = hashlib.sha256(Path(path).read_bytes()).hexdigest()
    Path(f"{path}.sha256").write_text(f"{digest}  {Path(path).name}\n", encoding="utf-8")


def archive_binary(binary, name, include, out, windows):
    """`out/<name>.zip` (Windows) or `.tar.gz`, holding `<name>/<binary>` and
    the included files. Returns the archive's path."""
    files = [Path(binary)] + [Path(f) for f in include]
    out.mkdir(parents=True, exist_ok=True)
    if windows:
        target = out / f"{name}.zip"
        with zipfile.ZipFile(target, "w", zipfile.ZIP_DEFLATED) as archive:
            for file in files:
                archive.write(file, f"{name}/{file.name}")
    else:
        target = out / f"{name}.tar.gz"
        with tarfile.open(target, "w:gz") as archive:
            for file in files:
                info = archive.gettarinfo(str(file), f"{name}/{file.name}")
                if file == Path(binary):
                    info.mode = 0o755
                with open(file, "rb") as handle:
                    archive.addfile(info, handle)
    return target


def prepare(job):
    """Run an artifact's `prepare` command before it is built (the build action
    calls this before maturin-action, which builds wheels by itself)."""
    job = json.loads(job) if isinstance(job, str) else job
    if command := job.get("prepare"):
        run(command, shell=True, env={"VERSION": job["version"], "PACKAGE": job["package"],
                                      "TARGET": job.get("target", "")})


def github_release(name, stage, notes_out, read=None, runner=None, options_only=False):
    """Shape a `github` package's release from release-kit.yml: write its
    `checksums` file into `stage` (over every file there but the `.sha256`
    ones), and run its `notes` command into `notes_out`. Returns
    {"notes": bool, "attest": bool, "checksums": name or ""}; with
    `options_only`, only returns it (a verify-only run)."""
    release = load(read or files_on_disk())
    package = next((p for p in release.packages if p.registry == "github" and p.name == name), None)
    if package is None:
        raise ValueError(f"{CONFIG} has no github package {name!r}")
    options = dict(package.github)
    stage = Path(stage)
    sums = options.get("checksums") or ""
    shape = {"notes": bool(options.get("notes")), "attest": options.get("attest") is True, "checksums": sums,
             "homebrew": dict(options.get("homebrew") or {}), "chocolatey": dict(options.get("chocolatey") or {})}
    if options_only:
        return shape
    if sums:
        files = sorted(p for p in stage.iterdir()
                       if p.is_file() and not p.name.endswith(".sha256") and p.name != sums)
        if not files:
            raise RuntimeError(f"{name}: no files in {stage} to list in {sums}")
        lines = [f"{hashlib.sha256(p.read_bytes()).hexdigest()}  {p.name}\n" for p in files]
        (stage / sums).write_text("".join(lines), encoding="utf-8")
    if command := options.get("notes"):
        result = (runner or subprocess.run)(
            bash_command(command), check=True, text=True, capture_output=True,
            env={**os.environ, "VERSION": package.version, "PACKAGE": package.name, "TAG": package.tag})
        if not result.stdout.strip():
            raise RuntimeError(f"{name}: the notes command printed nothing")
        Path(notes_out).write_text(result.stdout, encoding="utf-8")
    return shape


# ---------------------------------------------------------------------------
# Homebrew and Chocolatey: made from a published GitHub release

# `${name}` placeholders only, so a template's own `$var`, `#{var}` or
# PowerShell `${env:X}` is left alone.
PLACEHOLDER = re.compile(r"\$\{([a-z0-9_]+)\}")
SUM_LINE = re.compile(r"^([0-9a-f]{64}) [ *](\S+)$")


def parse_sums(text):
    """{file name: sha256} from `sha256sum` output (or .sha256 files, joined)."""
    sums = {}
    for line in text.splitlines():
        if not line.strip():
            continue
        match = SUM_LINE.match(line.strip())
        if not match:
            raise ValueError(f"not a sha256sum line: {line!r}")
        sums[PurePosixPath(match[2]).name] = match[1]
    return sums


def channel_values(package, sums, repo):
    """The placeholders a template can use: `version`, `tag`, `repository`,
    `base_url` (the release's download URL), and for each archive of a
    rust-binary (`<bin>-v<version>-<target>.tar.gz|.zip`) `url_<target>` and
    `sha256_<target>`, with the target's `-` as `_`."""
    base = f"https://github.com/{repo}/releases/download/{package.tag}"
    values = {"version": package.version, "tag": package.tag, "repository": repo, "base_url": base}
    archive = re.compile(rf"(.+?)-v{re.escape(package.version)}-([a-z0-9_]+(?:-[a-z0-9_]+){{2,3}})\.(?:tar\.gz|zip)")
    for name, sha in sorted(sums.items()):
        if match := archive.fullmatch(name):
            key = match[2].replace("-", "_")
            if f"sha256_{key}" in values:
                raise ValueError(f"two archives for {match[2]}: name the one each template means")
            values[f"sha256_{key}"] = sha
            values[f"url_{key}"] = f"{base}/{name}"
    return values


def render_template(text, values, where):
    """Fill every `${name}`; one without a value is an error, never left in."""
    def fill(match):
        if match[1] not in values:
            raise ValueError(f"{where}: no value for ${{{match[1]}}} (there are: {', '.join(sorted(values))})")
        return values[match[1]]
    return PLACEHOLDER.sub(fill, text)


def render_channel(name, channel, sums_text, out, read=None, repo=None):
    """Render a github package's `homebrew` formula or `chocolatey` package
    from its release's checksums into `out`. Returns the files written."""
    release = load(read or files_on_disk())
    package = next((p for p in release.packages if p.registry == "github" and p.name == name), None)
    settings = dict(package.github).get(channel) if package else None
    if not settings:
        raise ValueError(f"{CONFIG}: github package {name!r} has no {channel}")
    values = channel_values(package, parse_sums(sums_text), repo or repository())
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    if channel == "homebrew":
        template = Path(settings["formula"])
        formula = settings.get("name") or template.name.removesuffix(".tmpl").removesuffix(".rb")
        target = out / f"{formula}.rb"
        target.write_text(render_template(template.read_text(encoding="utf-8"), values, str(template)),
                          encoding="utf-8")
        return [target]
    source = Path(settings["package"])
    if not source.is_dir():
        raise ValueError(f"{name}: chocolatey package {source} is not a directory")
    written = []
    for file in sorted(f for f in source.rglob("*") if f.is_file()):
        relative = file.relative_to(source)
        target = out / str(relative).removesuffix(".tmpl")
        target.parent.mkdir(parents=True, exist_ok=True)
        if file.name.endswith(".tmpl"):
            target.write_text(render_template(file.read_text(encoding="utf-8"), values, str(file)),
                              encoding="utf-8")
        else:
            shutil.copy2(file, target)
        written.append(target)
    if len([f for f in written if f.suffix == ".nuspec"]) != 1:
        raise ValueError(f"{name}: chocolatey package {source} needs exactly one .nuspec (or .nuspec.tmpl)")
    return written


def open_tap_pr(repo, branch, base, title, body):
    """Open (or find) the tap's pull request with the GitHub CLI."""
    found = subprocess.run(["gh", "pr", "list", "--repo", repo, "--head", branch, "--state", "open",
                            "--json", "url", "--jq", '.[0].url // ""'],
                           check=True, text=True, capture_output=True).stdout.strip()
    if found:
        return found
    return subprocess.run(["gh", "pr", "create", "--repo", repo, "--base", base, "--head", branch,
                           "--title", title, "--body", body],
                          check=True, text=True, capture_output=True).stdout.strip()


def update_tap(tap, formula, repo, version, release_url, open_pr=open_tap_pr):
    """Put `formula` in the tap checkout `tap` as Formula/<file>, on the
    branch `<formula name>-<version>`, and open a pull request for it.
    Re-running replaces that branch and reuses the open pull request; a tap
    that already has the formula is left alone. Returns the PR URL or None."""
    tap, formula = Path(tap), Path(formula)
    name = formula.stem
    git_tap = lambda *args, **kw: git("-C", str(tap), *args, **kw)  # noqa: E731
    base = git_tap("rev-parse", "--abbrev-ref", "HEAD")
    (tap / "Formula").mkdir(exist_ok=True)
    shutil.copy2(formula, tap / "Formula" / formula.name)
    if not git_tap("status", "--porcelain", "--", f"Formula/{formula.name}"):
        print(f"the tap's {base} already has {name} {version}", flush=True)
        return None
    branch = f"{name}-{version}"
    git_tap("checkout", "--quiet", "-B", branch)
    git_tap("add", f"Formula/{formula.name}")
    git_tap("-c", "user.name=github-actions[bot]",
            "-c", "user.email=41898282+github-actions[bot]@users.noreply.github.com",
            "commit", "--quiet", "-m", f"{name} {version}")
    # The branch is the release's own, named for the version: a re-run replaces it.
    git_tap("push", "--quiet", "--force", "origin", f"HEAD:refs/heads/{branch}")
    url = open_pr(repo, branch, base, f"{name} {version}",
                  f"The Homebrew formula for [{name} {version}]({release_url}), rendered by its release run "
                  "(release-kit).")
    print(f"tap pull request for {name} {version}: {url}", flush=True)
    return url


def bash_command(command):
    """`command` as an argument list that runs it in bash. Commands run in bash
    everywhere (GitHub's Windows runners have Git's bash). Not `shell=True`
    with `executable=bash`: on Windows that still builds `<shell> /c "..."`,
    which bash cannot read."""
    bash = shutil.which("bash")
    if not bash:
        raise RuntimeError("bash is not on PATH")
    return [bash, "-c", command]


def run(command, cwd=None, env=None, shell=False):
    print(f"$ {command if shell else ' '.join(command)}", flush=True)
    subprocess.run(bash_command(command) if shell else command, cwd=cwd,
                   env={**os.environ, **(env or {})}, check=True)


class _NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def github_api(path, accept="application/vnd.github+json"):
    """(status, headers, body) of a GET on the GitHub API with the job's token,
    without following redirects: an artifact's /zip answers with a signed URL
    that must be fetched without the token."""
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    if not token:
        raise RuntimeError("ci-artifact needs GH_TOKEN (a token with actions: read) to read CI artifacts")
    request = Request(f"https://api.github.com{path}", headers={
        "User-Agent": USER_AGENT, "Accept": accept, "Authorization": f"Bearer {token}",
        "X-GitHub-Api-Version": "2022-11-28"})
    try:
        with build_opener(_NoRedirect).open(request, timeout=60) as response:
            return response.status, dict(response.headers), response.read()
    except HTTPError as error:
        return error.code, dict(error.headers or {}), error.read()


def download(url):
    """The bytes at a signed URL, fetched with no credentials."""
    with urlopen(Request(url, headers={"User-Agent": USER_AGENT}), timeout=300) as response:
        return response.read()


def tree_of(commit):
    """The tree of a commit in this repository, from the GitHub API: the CI
    run's commit need not be in the release checkout."""
    status, _, body = github_api(f"/repos/{repository()}/git/commits/{commit}")
    if status != 200:
        raise RuntimeError(f"cannot read commit {commit} of {repository()} (HTTP {status})")
    return json.loads(body)["tree"]["sha"]


def promote(job, out, tagged_tree=None):
    """Copy the files of one pinned CI artifact into `out`, unchanged, after
    proving they are the reviewed bytes of this release: the archive has the
    pinned SHA-256, has not expired, and was built from a commit whose tree is
    the tagged commit's tree (rule 4: a release is one commit's files)."""
    ident, pinned = job["artifact"], job["sha256"]
    where = f"{job['package']}: CI artifact {ident}"
    status, _, body = github_api(f"/repos/{repository()}/actions/artifacts/{ident}")
    if status != 200:
        raise RuntimeError(f"{where}: not found in {repository()} (HTTP {status})")
    meta = json.loads(body)
    if meta.get("expired"):
        raise RuntimeError(f"{where} has expired; pin a newer run's artifact")
    if meta.get("digest") and meta["digest"] != f"sha256:{pinned}":
        raise RuntimeError(f"{where}: GitHub records {meta['digest']}, release-kit.yml pins sha256:{pinned}")
    built = (meta.get("workflow_run") or {}).get("head_sha")
    if not built:
        raise RuntimeError(f"{where}: GitHub does not say which commit built it")
    tagged = tagged_tree or git("rev-parse", "HEAD^{tree}")
    if tree_of(built) != tagged:
        raise RuntimeError(f"{where} was built from {built[:12]}, whose files differ from the tagged commit's: "
                           "a release is one commit. Pin an artifact built from these files.")
    status, headers, body = github_api(f"/repos/{repository()}/actions/artifacts/{ident}/zip")
    location = {k.lower(): v for k, v in headers.items()}.get("location")
    if status in (301, 302, 303, 307, 308) and location:
        data = download(location)
    elif status == 200:
        data = body
    else:
        raise RuntimeError(f"{where}: download refused (HTTP {status})")
    if hashlib.sha256(data).hexdigest() != pinned:
        raise RuntimeError(f"{where}: the archive's SHA-256 is {hashlib.sha256(data).hexdigest()}, "
                           f"release-kit.yml pins {pinned}")
    version = pep440(job["version"]) if job["registry"] == "pypi" else job["version"]
    copied = []
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        for info in archive.infolist():
            name = info.filename
            if info.is_dir() or not fnmatch.fnmatchcase(name, job["files"]):
                continue
            parts = PurePosixPath(name).parts
            if name.startswith("/") or "\\" in name or ".." in parts:
                raise RuntimeError(f"{where}: unsafe path {name!r} in the archive")
            target = out / parts[-1]
            if target.exists():
                raise RuntimeError(f"{where}: two members are named {parts[-1]}")
            if version not in parts[-1]:
                raise RuntimeError(f"{where}: {parts[-1]} is not version {version} of {job['package']}")
            target.write_bytes(archive.read(info))
            copied.append(target)
    if not copied:
        raise RuntimeError(f"{where}: no member matches {job['files']!r}")
    print(f"promoted {where} ({built[:12]}): " + ", ".join(p.name for p in copied), flush=True)
    return copied


def build(job, out=Path("dist")):
    """Build one planned job into `out`; for GitHub Releases, add a .sha256
    beside every file. maturin jobs are built by maturin-action before this
    step, which then only adds the checksums."""
    job = json.loads(job) if isinstance(job, str) else job
    out = Path(out).resolve()
    out.mkdir(parents=True, exist_ok=True)
    before = set(out.iterdir())
    kind = job["kind"]
    if kind == "rust-binary":
        windows = "windows" in job["target"]
        run(["cargo", "build", "--release", "--locked", "-p", job["crate"], "--bin", job["bin"],
             "--target", job["target"]])
        binary = Path("target") / job["target"] / "release" / (job["bin"] + (".exe" if windows else ""))
        archive_binary(binary, f"{job['bin']}-v{job['version']}-{job['target']}", job.get("include", []),
                       out, windows)
    elif kind == "python":
        run([sys.executable, "-m", "pip", "install", "--quiet", "build"])
        run([sys.executable, "-m", "build", "--outdir", str(out), job["dir"]])
    elif kind == "vsix":
        install = ["npm", "ci"] if Path(job["dir"], "package-lock.json").exists() else ["npm", "install"]
        run(install, cwd=job["dir"])
        name = job["package"].split(".", 1)[-1]
        run(["npx", "--yes", "@vscode/vsce", "package", "--out", str(out / f"{name}-{job['version']}.vsix")],
            cwd=job["dir"])
    elif kind == "command":
        run(job["run"], shell=True, env={"VERSION": job["version"], "PACKAGE": job["package"], "OUT": str(out)})
        for path in Path().glob(job["outputs"]):
            if path.is_file() and path.resolve().parent != out:
                shutil.copy2(path, out / path.name)
    elif kind == "ci-artifact":
        promote(job, out)
    elif kind in ("maturin", "maturin-sdist"):
        # maturin-action wrote into <dir>/dist; collect what it left elsewhere.
        built = Path(job.get("dir") or ".") / "dist"
        if built.resolve() != out and built.is_dir():
            for path in built.iterdir():
                if path.is_file():
                    shutil.copy2(path, out / path.name)
    else:
        raise ValueError(f"unknown build kind {kind!r}")
    made = sorted(p for p in out.iterdir() if p not in before and not p.name.endswith(".sha256"))
    if kind in ("maturin", "maturin-sdist"):
        made = sorted(p for p in out.iterdir() if not p.name.endswith(".sha256"))
    if not made:
        raise RuntimeError(f"{job['id']} produced no files in {out}")
    if test := job.get("test"):
        # The artifact's own test, on what was just built, before anything
        # can publish it: a failure fails the build, and the publish jobs need it.
        run(test, shell=True, env={"DIST": str(out), "FILES": " ".join(str(p) for p in made),
                                   "VERSION": job["version"], "PACKAGE": job["package"],
                                   "TARGET": job.get("target", "")})
        print(f"tested {job['id']}", flush=True)
    if job["registry"] == "github":
        for path in made:
            sha256_file(path)
    print(f"built  {job['id']}: " + ", ".join(p.name for p in made), flush=True)
    return made


# ---------------------------------------------------------------------------
# Tags and runs


def remote_tags(remote):
    """{tag: commit} on the remote, peeled to commits."""
    tags = {}
    for line in git("ls-remote", "--tags", remote).splitlines():
        sha, ref = line.split("\t")
        name = ref.removeprefix("refs/tags/")
        if name.endswith("^{}"):
            tags[name[:-3]] = sha
        else:
            tags.setdefault(name, sha)
    return tags


def lightweight(remote, names):
    """The tags among `names` that are lightweight on the remote (no ^{} line)."""
    peeled = {line.split("\t")[1].removeprefix("refs/tags/")[:-3]
              for line in git("ls-remote", "--tags", remote).splitlines() if line.endswith("^{}")}
    return {n for n in names if n not in peeled}


def have_commits(existing, remote):
    """Fetch tagged commits the local clone lacks, so their files can be read."""
    missing = [c for c in set(existing.values())
               if subprocess.run(["git", "cat-file", "-e", f"{c}^{{commit}}"], cwd=root(),
                                 capture_output=True).returncode]
    if missing:
        git("fetch", "--quiet", remote, *missing)


def run_of(tag_name):
    """The latest release run for a tag: status, conclusion, url, head_sha and
    whether a publish job started. None without `gh` or without a run."""
    if not shutil.which("gh"):
        return None
    result = subprocess.run(
        ["gh", "run", "list", "--branch", tag_name, "--limit", "1",
         "--json", "status,conclusion,url,headSha,databaseId",
         "--jq", '.[0] | "\\(.status) \\(.conclusion) \\(.url) \\(.headSha) \\(.databaseId)"'],
        cwd=root(), text=True, capture_output=True)
    fields = result.stdout.split()
    if result.returncode or len(fields) != 5:
        return None
    status, conclusion, url, head_sha, run_id = fields
    jobs = subprocess.run(
        ["gh", "api", f"repos/{{owner}}/{{repo}}/actions/runs/{run_id}/jobs",
         "--jq", '[.jobs[] | select((.name | startswith("publish")) and .status != "queued"'
                 ' and .conclusion != "skipped")] | length'],
        cwd=root(), text=True, capture_output=True)
    # Unknown counts as started: a publish job may have uploaded.
    started = jobs.returncode != 0 or jobs.stdout.strip() != "0"
    return {"status": status, "conclusion": conclusion, "url": url, "head_sha": head_sha,
            "publish_started": started}


def dead(run, tagged):
    """Whether a run proves nothing was published from `tagged`: it ran there,
    ended without success, and no publish job started."""
    return bool(run and run["head_sha"] == tagged and run["status"] == "completed"
                and run["conclusion"] != "success" and not run["publish_started"])


def tag_subject(name, remote="origin"):
    """The first line of an annotated tag's message, fetched from `remote`
    (it may have moved); "" for a lightweight tag."""
    ref = f"refs/tags/{name}"
    git("fetch", "--quiet", "--no-tags", remote, f"+{ref}:{ref}")
    if git("cat-file", "-t", ref) != "tag":
        return ""
    return git("for-each-ref", ref, "--format=%(contents:subject)")


def not_its_own(release, existing, sha, remote, packages=None):
    """Package tags already on the remote at another commit that were not
    made for that package: `tag` writes `<name> <version> (<registry>)` as a
    record's first line. Another package's tag with the same name (a crate and
    a PyPI package called alike), or one made by other tooling, is not this
    package's record."""
    found = []
    for package in packages if packages is not None else release.packages:
        tagged = existing.get(package.tag)
        if tagged and tagged != sha and tag_subject(package.tag, remote) != str(package):
            found.append(package)
    return found


def foreign_error(packages, existing):
    lines = [f"  {p.tag} at {existing[p.tag][:12]}: not {p}'s record" for p in packages]
    return ("these tags already exist for something else:\n" + "\n".join(lines) + "\n"
            "Give the package a tag format of its own in release-kit.yml (for example "
            "`tag: \"python-v{version}\"` for a PyPI package named like a crate), or bump its version.")


def release_at(commit, package=None):
    """The release configured at `commit`; a commit without release-kit.yml
    (a tag from before the repository adopted release-kit) is refused."""
    try:
        return load(files_at(commit))
    except subprocess.CalledProcessError:
        what = f"{package.tag} is on {commit[:12]}, which" if package else f"{commit[:12]}"
        raise RuntimeError(f"{what} has no {CONFIG}: the tag predates release-kit. Bump the package's "
                           "version, or delete the stale tag if nothing was published from it.") from None


def changed_files(package, old, new):
    names = git("diff", "--name-only", old, new).splitlines()
    return [n for n in names if any(fnmatch.fnmatch(n, pattern) for pattern in package.shipped)]


def split_from(release, existing, sha, binds):
    """Packages tagged on another commit whose shipped files differ at `sha`:
    releasing `sha` would put other code under their numbers. Returns
    {commit: [description]}."""
    found = {}
    for package in release.packages:
        tagged = existing.get(package.tag)
        if not tagged or tagged == sha or not binds(package, tagged):
            continue
        if changed := changed_files(package, tagged, sha):
            shown = ", ".join(changed[:3]) + (", ..." if len(changed) > 3 else "")
            found.setdefault(tagged, []).append(f"{package} ({shown})")
    return found


def split_error(sha, split):
    lines = [f"  tagged at {commit[:12]}: " + "; ".join(names) for commit, names in split.items()]
    hint = " or ".join(f"--commit {commit[:12]}" for commit in split)
    return (f"{sha[:12]} changes packages already tagged under the same version on other commits:\n"
            + "\n".join(lines) + f"\nRelease from that commit ({hint}), or bump those packages first.")


def tag_message(subject, release, sha):
    return f"{subject}\n\nRelease: {release.name}\nCommit: {sha}\n"


def trailers(tag_name):
    text = git("for-each-ref", f"refs/tags/{tag_name}", "--format=%(contents)")
    return dict(re.findall(r"(?m)^(Release|Commit): (\S+)$", text))


# ---------------------------------------------------------------------------
# The gate (CI)


def gate(tag_name, remote="origin", status=published_on, output=None, verify_only=False):
    """Everything a release run must hold before building, from the checked-out
    tag. Returns the outputs written to GITHUB_OUTPUT."""
    ref = f"refs/tags/{tag_name}"
    if git("cat-file", "-t", ref) != "tag":
        raise RuntimeError(f"{tag_name} must be an annotated tag (release.py tag makes them)")
    sha = git("rev-parse", "--verify", f"{ref}^{{commit}}")
    read = files_at(sha)
    if errors := check(read):
        raise RuntimeError(f"{CONFIG} at {sha[:12]} disagrees with the repository:\n  " + "\n  ".join(errors))
    release = load(read)
    if tag_name != release.tag:
        raise RuntimeError(f"{tag_name} is not this commit's release tag ({release.tag}); "
                           "a release run starts only from its release tag")
    git("fetch", "--quiet", remote, release.branch)
    if subprocess.run(["git", "merge-base", "--is-ancestor", sha, f"{remote}/{release.branch}"],
                      cwd=root()).returncode:
        raise RuntimeError(f"{tag_name} points at {sha[:12]}, which is not on {release.branch}: "
                           "release tags go on the default branch after the release PR merges")
    recorded = trailers(tag_name)
    if recorded.get("Commit") != sha or recorded.get("Release") != release.name:
        raise RuntimeError(f"{tag_name} must record `Release: {release.name}` and `Commit: {sha}` "
                           "in its message (release.py tag writes them)")
    existing = remote_tags(remote)
    have_commits(existing, remote)
    unpublished = [p for p in release.packages if not status(p)]
    if foreign := not_its_own(release, existing, sha, remote, unpublished):
        raise RuntimeError(foreign_error(foreign, existing))
    if verify_only:
        return verify_selection(tag_name, release, sha, existing, status, output)
    if split := split_from(release, existing, sha, lambda package, tagged: True):
        raise RuntimeError(split_error(sha, split))
    due = []
    for package in release.packages:
        if status(package):
            print(f"{package}: already published; not selected", flush=True)
        else:
            print(f"{package}: not yet published; selected", flush=True)
            due.append(package)
    if not due:
        raise RuntimeError(f"{tag_name}: every package of release {release.name} is already published")
    untagged = [f"{p.tag} ({'missing' if p.tag not in existing else 'at ' + existing[p.tag][:12]})"
                for p in due if existing.get(p.tag) != sha]
    if untagged:
        raise RuntimeError(f"each selected package's tag must be on {sha[:12]}: {', '.join(untagged)}. "
                           "release.py tag pushes them.")
    if plain := lightweight(remote, [p.tag for p in due]):
        raise RuntimeError(f"record tags must be annotated: {', '.join(sorted(plain))}")
    outputs = gate_outputs(sha, release, due, False, output)
    print(f"{tag_name} -> {sha} (release {release.name}): publishing "
          + ", ".join(str(p) for p in due), flush=True)
    return outputs


def gate_outputs(sha, release, chosen, verify_only, output=None):
    outputs = {
        "sha": sha,
        "release": release.name,
        "selection": json.dumps([{"name": p.name, "registry": p.registry, "version": p.version,
                                  "manifest": p.manifest, "tag": p.tag, "slug": p.slug} for p in chosen]),
        "verify-only": str(verify_only).lower(),
    }
    for registry in REGISTRIES:
        outputs[registry.replace(".", "-")] = str(any(p.registry == registry for p in chosen)).lower()
    # Channels made from a GitHub release: whether a selected github package has one.
    for channel in CHANNEL_KEYS:
        outputs[channel] = str(any(p.registry == "github" and dict(p.github).get(channel)
                                   for p in chosen)).lower()
    target = output or os.environ.get("GITHUB_OUTPUT")
    if target:
        with open(target, "a", encoding="utf-8") as handle:
            for key, value in outputs.items():
                handle.write(f"{key}={value}\n")
    return outputs


def verify_selection(tag_name, release, sha, existing, status, output):
    """A verify-only run: the packages this release tagged on `sha`, every
    one already published. Builds and uploads nothing; the publish jobs only
    verify. It cannot finish a partial upload: run `release.py tag` for that."""
    mine = [p for p in release.packages if existing.get(p.tag) == sha]
    if not mine:
        raise RuntimeError(f"{tag_name}: no package of release {release.name} is tagged on {sha[:12]}")
    if missing := [str(p) for p in mine if not status(p)]:
        raise RuntimeError(f"a verify-only run checks a finished upload, but these are not published: "
                           f"{', '.join(missing)}. Run `release.py tag` again to publish them.")
    print(f"{tag_name} -> {sha} (release {release.name}): verifying " + ", ".join(str(p) for p in mine),
          flush=True)
    return gate_outputs(sha, release, mine, True, output)


def selected(registry, selection):
    """The packages of `registry` in a gate's selection (JSON), in order. The
    selection carries each package's rendered tag, which serves as its format."""
    return [Package(s["name"], s["registry"], s["manifest"], s["version"], s.get("tag", "{name}-v{version}"))
            for s in json.loads(selection) if s["registry"] == registry]


def crate_exists(name):
    """Whether crates.io has the crate at all (any version)."""
    return http_status(f"https://crates.io/api/v1/crates/{quote(name)}")[0] == 200


def preflight(registry, selection, status=published_on, exists=crate_exists, output=None):
    """Refuse, just before uploading, any selected version already published:
    another run may have published it since the gate. For crates.io, also
    say which selected crates are new (their first version needs a token,
    since Trusted Publishing needs the crate to exist): writes `new_crates`
    (JSON), `new_crate` and `existing_crate` to GITHUB_OUTPUT."""
    chosen = selected(registry, selection)
    if out := [str(p) for p in chosen if status(p)]:
        raise RuntimeError(f"already on {registry}: {', '.join(out)}; a published version is permanent")
    print(f"to publish on {registry}: " + (", ".join(str(p) for p in chosen) or "nothing"), flush=True)
    if registry == "crates.io":
        new = [p.name for p in chosen if not exists(p.name)]
        outputs = {"new_crates": json.dumps(new), "new_crate": str(bool(new)).lower(),
                   "existing_crate": str(len(new) < len(chosen)).lower()}
        target = output or os.environ.get("GITHUB_OUTPUT")
        if target:
            with open(target, "a", encoding="utf-8") as handle:
                for key, value in outputs.items():
                    handle.write(f"{key}={value}\n")
        if new:
            print("first versions (published with the new-crate token): " + ", ".join(new), flush=True)
    return chosen


def publish_groups(chosen, new):
    """[(credential, [crates])]: consecutive runs of `chosen`, in order, that
    share a credential ("trusted" for crates already on crates.io, "new" for
    first versions). Order is kept, so each crate follows what it depends on."""
    groups = []
    for package in chosen:
        credential = "new" if package.name in new else "trusted"
        if groups and groups[-1][0] == credential:
            groups[-1][1].append(package.name)
        else:
            groups.append((credential, [package.name]))
    return groups


# crates.io limits how many new crates one account publishes: a burst, then
# one every few minutes. Its 429 says when to try again.
RATE_LIMITED = re.compile(r"429|too many new crates|try again after", re.I)
RETRY_AFTER = re.compile(r"try again after ([A-Z][a-z]{2}, \d{1,2} [A-Z][a-z]{2} \d{4} \d{2}:\d{2}:\d{2} GMT)")
NEW_CRATE_WAIT = 600


def run_capturing(command, env=None):
    """`run`, also keeping what the command printed: a failure's
    CalledProcessError carries it as `output`."""
    print(f"$ {' '.join(command)}", flush=True)
    result = subprocess.run(command, env={**os.environ, **(env or {})}, text=True,
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    print(result.stdout, end="", flush=True)
    if result.returncode:
        raise subprocess.CalledProcessError(result.returncode, command, output=result.stdout)


def rate_limit_wait(output, now=None):
    """Seconds to wait before publishing another new crate, from crates.io's
    429 message, or None if `output` is not a rate limit."""
    if not RATE_LIMITED.search(output or ""):
        return None
    if match := RETRY_AFTER.search(output):
        from email.utils import parsedate_to_datetime
        at = parsedate_to_datetime(match[1]).timestamp()
        return max(5, min(3600, at - (now or time.time()) + 5))
    return NEW_CRATE_WAIT


def publish_new_crate(crate, token, runner, sleep=time.sleep, tries=30):
    """`cargo publish` one new crate, waiting out crates.io's new-crate rate
    limit as often as it takes (a first release of many crates hits it)."""
    for attempt in range(1, tries + 1):
        try:
            runner(["cargo", "publish", "--locked", "-p", crate], env={"CARGO_REGISTRY_TOKEN": token})
            return
        except subprocess.CalledProcessError as error:
            wait = rate_limit_wait(error.output)
            if wait is None or attempt == tries:
                raise
            print(f"crates.io rate-limits new crates: {crate} waits {int(wait)}s", flush=True)
            sleep(wait)


def publish_crates(selection, new_crates, trusted_token, new_token, runner=None, sleep=time.sleep):
    """Publish the selected crates: one `cargo publish --dry-run` for all of
    them, then one `cargo publish` per run of crates already on crates.io,
    and one per new crate (each waiting out the new-crate rate limit).
    Refuses before uploading anything if a group's token is missing."""
    upload = runner or run_capturing
    runner = runner or run
    chosen = selected("crates.io", selection)
    new = set(json.loads(new_crates or "[]"))
    groups = publish_groups(chosen, new)
    tokens = {"trusted": trusted_token, "new": new_token}
    if missing := [c for c, _ in groups if not tokens[c]]:
        need = {"trusted": "the Trusted Publishing token (rust-lang/crates-io-auth-action)",
                "new": "the new-crate token (the environment's CARGO_REGISTRY_TOKEN secret)"}
        raise RuntimeError("cannot publish: missing " + " and ".join(need[c] for c in dict.fromkeys(missing)))
    if not chosen:
        print("nothing to publish on crates.io", flush=True)
        return groups
    every = [arg for p in chosen for arg in ("-p", p.name)]
    runner(["cargo", "publish", "--dry-run", "--locked", *every], env={"CARGO_REGISTRY_TOKEN": ""})
    for credential, crates in groups:
        if credential == "new":
            for crate in crates:
                publish_new_crate(crate, tokens["new"], upload, sleep)
        else:
            runner(["cargo", "publish", "--locked", *[a for c in crates for a in ("-p", c)]],
                   env={"CARGO_REGISTRY_TOKEN": tokens[credential]})
    return groups


def verify(registry, selection, status=published_on, timeout=900, sleep=time.sleep):
    """Wait until every selected package of `registry` is on it."""
    chosen = selected(registry, selection)
    deadline = time.monotonic() + timeout
    while missing := [p for p in chosen if not status(p)]:
        if time.monotonic() > deadline:
            raise RuntimeError(f"not on {registry} after {timeout // 60} minutes: "
                               + ", ".join(str(p) for p in missing))
        sleep(20)
    print(f"on {registry}: " + (", ".join(str(p) for p in chosen) or "nothing selected"), flush=True)


def has_binary(manifest, read=None):
    """Whether a Cargo package builds a binary: a [[bin]] table, or the
    default src/main.rs beside its manifest."""
    read = read or files_on_disk()
    if tomllib.loads(read(manifest)).get("bin"):
        return True
    try:
        read(str(PurePosixPath(manifest).parent / "src" / "main.rs"))
        return True
    except (FileNotFoundError, subprocess.CalledProcessError):
        return False


def consume(registry, selection, runner=None, read=None, tries=10, sleep=time.sleep):
    """Use each selected package as a stranger would, from its registry, in a
    fresh temporary directory with no registry credentials: crates.io installs
    a crate with a binary at its exact version (`cargo install --locked`) and
    checks that a new crate builds against a library; PyPI installs the version
    into a new virtualenv; npm resolves it. A registry's index can lag behind
    its API, so each attempt is retried."""
    runner = runner or run
    chosen = selected(registry, selection)
    clean = {"CARGO_REGISTRY_TOKEN": "", "NODE_AUTH_TOKEN": "", "PYPI_API_TOKEN": ""}

    def attempt(step, describe):
        for n in range(tries):
            try:
                step()
                return
            except subprocess.CalledProcessError:
                if n == tries - 1:
                    raise RuntimeError(f"{describe} failed {tries} times") from None
                sleep(30)

    for package in chosen:
        with tempfile.TemporaryDirectory(prefix="release-kit-consume-") as scratch:
            here = Path(scratch)
            if registry == "crates.io" and has_binary(package.manifest, read):
                attempt(lambda: runner(["cargo", "install", package.name, "--version", f"={package.version}",
                                        "--locked", "--root", str(here / "root")], cwd=scratch, env=clean),
                        f"installing {package}")
            elif registry == "crates.io":
                runner(["cargo", "new", "--quiet", "--lib", "consumer"], cwd=scratch, env=clean)
                consumer = str(here / "consumer")
                attempt(lambda: runner(["cargo", "add", f"{package.name}@={package.version}"], cwd=consumer,
                                       env=clean), f"adding {package}")
                runner(["cargo", "check", "--quiet"], cwd=consumer, env=clean)
            elif registry == "pypi":
                runner([sys.executable, "-m", "venv", str(here / "venv")], env=clean)
                python = here / "venv" / ("Scripts/python.exe" if os.name == "nt" else "bin/python")
                attempt(lambda: runner([str(python), "-m", "pip", "install", "--quiet", "--no-cache-dir",
                                        f"{package.name}=={package.version}"], env=clean),
                        f"installing {package}")
            elif registry == "npm":
                attempt(lambda: runner(["npm", "view", f"{package.name}@{package.version}", "version"],
                                       cwd=scratch, env=clean), f"resolving {package}")
            else:
                continue  # github, vscode: `verify` has already seen the files
            print(f"used   {package} as a consumer would", flush=True)
    return chosen


# ---------------------------------------------------------------------------
# Installing: repositories use a copy of the kit, pinned and reviewable: a
# repository's release changes only when it installs a newer kit.

KIT = Path(__file__).resolve().parent.parent
VENDOR = ".github/release-kit"
BUILD_WORKFLOW = ".github/workflows/release-kit-build.yml"
MANIFEST = f"{VENDOR}/manifest.json"
NOTICE = """# release-kit (installed copy)

Installed from buchochelliq-labs/release-kit {version} ({commit}) by
`release.py install`. Do not edit these files: CI checks them against
manifest.json (`python3 .github/release-kit/scripts/release.py vendored`).
To update, install again from a newer release-kit checkout:

    python3 ../release-kit/scripts/release.py install

This repository's release is configured in release-kit.yml at its root.
"""


def kit_files(kit=KIT):
    """{path in the repository: source file} for an install."""
    files = {f"{VENDOR}/scripts/release.py": kit / "scripts" / "release.py",
             BUILD_WORKFLOW: kit / "workflows" / "release-kit-build.yml"}
    for action in sorted(kit.glob("*/action.yml")):
        files[f"{VENDOR}/{action.parent.name}/action.yml"] = action
    return files


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def install(target=None, kit=KIT):
    """Copy the kit into the repository at `target` (default: the one you are
    in). Returns the paths written."""
    if not (kit / "workflows" / "release-kit-build.yml").exists():
        raise RuntimeError("install runs from a release-kit checkout, not from an installed copy: "
                           "git clone https://github.com/buchochelliq-labs/release-kit")
    target = Path(target) if target else root()
    if target.resolve() == kit.resolve():
        raise RuntimeError("run install from the repository to install into, not from release-kit itself")
    version = subprocess.run(["git", "describe", "--tags", "--always", "--dirty"], cwd=kit, text=True,
                             capture_output=True).stdout.strip() or "unversioned"
    commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=kit, text=True, capture_output=True).stdout.strip()
    vendor = target / VENDOR
    if vendor.exists():
        shutil.rmtree(vendor)  # a clean copy: files the new version dropped go too
    written = []
    for path, source in kit_files(kit).items():
        destination = target / path
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(source, destination)
        written.append(path)
    (vendor / "README.md").write_text(NOTICE.format(version=version, commit=commit[:12]), encoding="utf-8")
    manifest = {"source": "buchochelliq-labs/release-kit", "version": version, "commit": commit,
                "files": {path: digest(target / path) for path in sorted(written)}}
    (target / MANIFEST).write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    written += [f"{VENDOR}/README.md", MANIFEST]
    starter = target / ".github" / "workflows" / "release.yml"
    if not starter.exists():
        shutil.copy2(kit / "templates" / "release.yml", starter)
        written.append(".github/workflows/release.yml")
    print(f"installed release-kit {version} into {target}:", flush=True)
    for path in written:
        print(f"  {path}", flush=True)
    return written


def vendored(target=None):
    """Problems with the installed copy: files edited, missing, or added."""
    target = Path(target) if target else root()
    try:
        manifest = json.loads((target / MANIFEST).read_text(encoding="utf-8"))
    except FileNotFoundError:
        return [f"{MANIFEST} is missing: install release-kit (release.py install)"]
    problems = []
    for path, expected in manifest["files"].items():
        file = target / path
        if not file.exists():
            problems.append(f"{path} is missing")
        elif digest(file) != expected:
            problems.append(f"{path} was edited; release-kit's files change only by installing again")
    known = set(manifest["files"]) | {MANIFEST, f"{VENDOR}/README.md"}
    for file in sorted((target / VENDOR).rglob("*")):
        relative = file.relative_to(target).as_posix()
        if file.is_file() and relative not in known and "__pycache__" not in relative:
            problems.append(f"{relative} is not part of release-kit {manifest['version']}")
    return problems


# ---------------------------------------------------------------------------
# Tagging (person)


def releases_tagged(existing, release_tag_pattern):
    """{commit: Release} for commits carrying a release tag."""
    found = {}
    for name, commit in existing.items():
        if re.fullmatch(release_tag_pattern, name):
            try:
                found[commit] = load(files_at(commit))
            except (subprocess.CalledProcessError, ValueError, tomllib.TOMLDecodeError):
                continue  # not a release-kit release (from before adoption): not resumed
    return found


def resolve(commit, wanted, remote, existing, binds, published):
    """The commit to release: --commit; else where an unfinished release
    (--release, or the newest tagged) was started; else, for --release, the
    last commit on the default branch naming it; else HEAD."""
    if commit:
        return git("rev-parse", "--verify", f"{commit}^{{commit}}"), "--commit"
    here = load(files_on_disk())
    pattern = re.escape(here.tag_format).replace(r"\{release\}", ".+")
    have_commits(existing, remote)
    tagged = releases_tagged(existing, pattern)
    target = wanted or max((r.name for r in tagged.values()), key=version_key, default=None)
    for candidate, release in tagged.items():
        if release.name != target:
            continue
        mine = [p for p in release.packages if existing.get(p.tag) == candidate]
        if not any(binds(p, candidate) for p in mine) or split_from(release, existing, candidate, binds):
            continue
        if wanted or any(not published(p) for p in release.packages):
            return candidate, f"release {target} is already tagged there"
    if wanted:
        branch = here.branch
        for candidate in git("rev-list", "--first-parent", f"{remote}/{branch}").splitlines():
            try:
                name = load(files_at(candidate)).name
            except (subprocess.CalledProcessError, ValueError, tomllib.TOMLDecodeError):
                break
            if name == wanted:
                return candidate, f"the last commit on {remote}/{branch} naming release {wanted}"
            if version_key(name) < version_key(wanted):
                break
        raise RuntimeError(f"no commit on {remote}/{branch} names release {wanted}")
    return git("rev-parse", "--verify", "HEAD^{commit}"), "the local checkout (HEAD)"


def local_tag(name):
    return subprocess.run(["git", "rev-parse", "--verify", "--quiet", f"refs/tags/{name}^{{commit}}"],
                          cwd=root(), text=True, capture_output=True).stdout.strip()


def wait_for(tag_name, packages, timeout, status=published_on, run=run_of, sleep=time.sleep, stale=None):
    """Return once the release run succeeded and every package is published."""
    deadline = time.monotonic() + timeout
    while True:
        missing = [p for p in packages if not status(p)]
        latest = run(tag_name)
        if latest and latest["url"] == stale:
            latest = None
        if latest and latest["status"] == "completed" and latest["conclusion"] != "success":
            raise RuntimeError(f"the {tag_name} run ended {latest['conclusion']}: {latest['url']} "
                               f"({len(packages) - len(missing)} of {len(packages)} published). Fix the cause, "
                               "then run `release.py tag` again: it pushes the tag again and the new run "
                               "publishes only what is still due.")
        if not missing and (latest is None or latest["status"] == "completed"):
            print(f"{tag_name}: published " + ", ".join(str(p) for p in packages), flush=True)
            return
        if time.monotonic() > deadline:
            still = latest and latest["status"] != "completed"
            raise RuntimeError(f"not published within {timeout // 60} minutes: "
                               + ", ".join(str(p) for p in missing)
                               + (f". The run is still going ({latest['url']}); run `release.py tag` again "
                                  "to keep waiting: it changes nothing while the run is live." if still else ""))
        sleep(30)


def tag(commit=None, remote="origin", timeout=10800, dry_run=False, wanted=None,
        status=published_on, run=run_of, wait=wait_for):
    """Tag the release on its commit and wait for it to be published."""
    existing = remote_tags(remote)

    def binds(package, tagged):
        # A tag elsewhere holds the release there unless its run provably
        # published nothing; then the tag is moved.
        return status(package) or not dead(run(release_at(tagged, package).tag), tagged)

    here = load(files_on_disk())
    unpublished = [p for p in here.packages if not status(p)]
    if foreign := not_its_own(here, existing, None, remote, unpublished):
        raise RuntimeError(foreign_error(foreign, existing))

    sha, why = resolve(commit, wanted, remote, existing, binds, status)
    read = files_at(sha)
    release = load(read)
    if subprocess.run(["git", "merge-base", "--is-ancestor", sha, f"{remote}/{release.branch}"],
                      cwd=root()).returncode:
        raise RuntimeError(f"{sha[:12]} is not on {remote}/{release.branch} as last fetched: release from the "
                           "default branch after the release PR merges (git fetch first)")
    if errors := check(read):
        raise RuntimeError(f"{CONFIG} at {sha[:12]} disagrees with the repository:\n  " + "\n  ".join(errors))
    have_commits(existing, remote)
    if split := split_from(release, existing, sha, binds):
        raise RuntimeError(split_error(sha, split))
    print(f"Release {release.name} from {sha} ({why})", flush=True)

    deletions, records, due = [], [], []
    for package in release.packages:
        if status(package):
            print(f"skip   {package.tag}: {package} is already published", flush=True)
            continue
        tagged = existing.get(package.tag)
        if tagged and tagged != sha:
            if binds(package, tagged):
                # Tagged elsewhere, shipping the same files (the split check
                # passed), and its run there may publish it: wait for that run.
                print(f"wait   {package.tag}: tagged at {tagged[:12]}, which ships the same files; "
                      "waiting for its release run", flush=True)
                if not dry_run:
                    wait(release_at(tagged, package).tag, [package], timeout, status)
                continue
            print(f"move   {package.tag}: the release run at {tagged[:12]} published nothing", flush=True)
            deletions.append(package.tag)
            tagged = None
        if tagged:
            print(f"keep   {package.tag}: already tagged here", flush=True)
        else:
            print(f"tag    {package.tag} at {sha[:12]} (a record)", flush=True)
            records.append((package.tag, tag_message(str(package), release, sha)))
        due.append(package)
    if not due:
        print(f"Every package of release {release.name} is published.")
        return

    start, stale = None, None
    tagged = existing.get(release.tag)
    latest = run(release.tag) if tagged else None
    if tagged and tagged != sha:
        if not dead(latest, tagged):
            raise RuntimeError(f"{release.tag} is on {tagged[:12]}, not {sha[:12]}, and its run there may have "
                               f"published: release from that commit (--commit {tagged[:12]}) or wait for it")
        print(f"move   {release.tag}: its run at {tagged[:12]} ended {latest['conclusion']} before publishing",
              flush=True)
        deletions.append(release.tag)
        tagged = None
    elif tagged and latest and latest["status"] == "completed" and latest["conclusion"] != "success":
        print(f"retag  {release.tag}: its run ended {latest['conclusion']}; the new run publishes only what "
              "is still due", flush=True)
        deletions.append(release.tag)
        stale, tagged = latest["url"], None
    if tagged:
        print(f"wait   {release.tag}: already tagged here", flush=True)
    else:
        subjects = "".join(f"{p}\n" for p in due)
        start = (release.tag, tag_message(f"Release {release.name}\n\n{subjects}".rstrip(), release, sha))
        print(f"tag    {release.tag} at {sha[:12]} (starts the release run: "
              + ", ".join(p.name for p in due) + ")", flush=True)

    if dry_run:
        print("Dry run: nothing was tagged.")
        return
    if deletions:
        git("push", "--atomic", remote, *(f":refs/tags/{t}" for t in deletions), capture=False)
    for tag_name, message in records + ([start] if start else []):
        if local_tag(tag_name):
            git("tag", "-d", tag_name)
        git("tag", "-a", tag_name, "-m", message, sha)
    # Records first, in one atomic push; then the release tag alone, since
    # GitHub starts no workflow for a push of more than three tags.
    if records:
        git("push", "--atomic", remote, *(f"refs/tags/{t}" for t, _ in records), capture=False)
    if start:
        git("push", remote, f"refs/tags/{start[0]}", capture=False)
        print(f"pushed {start[0]}; waiting for the release run", flush=True)
    wait(release.tag, due, timeout, status, stale=stale)


def status_lines(read=None, status=published_on, remote="origin"):
    release = load(read or files_on_disk())
    try:
        existing = remote_tags(remote)
    except subprocess.CalledProcessError:
        existing = {}
    lines = [f"Release {release.name} (tag {release.tag}: "
             f"{existing[release.tag][:12] if release.tag in existing else 'not tagged'})"]
    for package in release.packages:
        tagged = existing.get(package.tag)
        lines.append(f"  {str(package):<48} {package.tag:<28} "
                     f"{'tagged ' + tagged[:12] if tagged else 'untagged':<20} "
                     f"{'published' if status(package) else 'due'}")
    return lines


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("check")
    commands.add_parser("sync")
    commands.add_parser("status")
    tagger = commands.add_parser("tag")
    tagger.add_argument("--commit")
    tagger.add_argument("--release", dest="wanted")
    tagger.add_argument("--remote", default="origin")
    tagger.add_argument("--timeout", type=int, default=10800,
                        help="seconds to wait for the run (default 3 hours); tag again to keep waiting")
    tagger.add_argument("--dry-run", action="store_true")
    gater = commands.add_parser("gate")
    gater.add_argument("tag")
    gater.add_argument("--remote", default="origin")
    gater.add_argument("--verify-only", action="store_true",
                       help="select the release's published packages, to verify them again")
    planner = commands.add_parser("plan")
    planner.add_argument("--selection", required=True)
    preparer = commands.add_parser("prepare")
    preparer.add_argument("--job", required=True)
    shaper = commands.add_parser("github-release")
    shaper.add_argument("--name", required=True)
    shaper.add_argument("--stage", required=True, help="the directory holding the release's files")
    shaper.add_argument("--notes-out", required=True, help="where to write the notes, if the package has some")
    shaper.add_argument("--options-only", action="store_true", help="only print what release-kit.yml says")
    renderer = commands.add_parser("render")
    renderer.add_argument("--name", required=True)
    renderer.add_argument("--channel", required=True, choices=sorted(CHANNEL_KEYS))
    renderer.add_argument("--sums", required=True, help="the release's checksums, in sha256sum format")
    renderer.add_argument("--out", required=True)
    tapper = commands.add_parser("homebrew-tap")
    tapper.add_argument("--tap", required=True, help="a checkout of the tap")
    tapper.add_argument("--formula", required=True)
    tapper.add_argument("--repo", required=True, help="the tap, owner/name")
    tapper.add_argument("--version", required=True)
    tapper.add_argument("--release-url", required=True)
    builder = commands.add_parser("build")
    builder.add_argument("--job", required=True)
    builder.add_argument("--out", default="dist")
    installer = commands.add_parser("install")
    installer.add_argument("--into", help="repository to install into (default: the one you are in)")
    commands.add_parser("vendored")
    pre = commands.add_parser("preflight")
    pre.add_argument("--registry", required=True, choices=REGISTRIES)
    pre.add_argument("--selection", required=True)
    verifier = commands.add_parser("verify")
    verifier.add_argument("--registry", required=True, choices=REGISTRIES)
    verifier.add_argument("--selection", required=True)
    verifier.add_argument("--timeout", type=int, default=900)
    consumer = commands.add_parser("consume")
    consumer.add_argument("--registry", required=True, choices=REGISTRIES)
    consumer.add_argument("--selection", required=True)
    publisher = commands.add_parser("publish-crates")
    publisher.add_argument("--selection", required=True)
    publisher.add_argument("--new-crates", default="[]", help="preflight's new_crates output (JSON)")
    args = parser.parse_args(argv)
    try:
        if args.command == "check":
            if errors := check(files_on_disk()):
                print("\n".join(errors), file=sys.stderr)
                return 1
            print(f"{CONFIG} matches the repository (release {load(files_on_disk()).name}).")
        elif args.command == "sync":
            print("\n".join(sync()) or "Nothing to change.")
        elif args.command == "status":
            print("\n".join(status_lines()))
        elif args.command == "tag":
            tag(args.commit, args.remote, args.timeout, args.dry_run, args.wanted)
        elif args.command == "gate":
            gate(args.tag, args.remote, verify_only=args.verify_only)
        elif args.command == "plan":
            plan(args.selection)
        elif args.command == "prepare":
            prepare(args.job)
        elif args.command == "github-release":
            print(json.dumps(github_release(args.name, args.stage, args.notes_out,
                                            options_only=args.options_only)))
        elif args.command == "render":
            for path in render_channel(args.name, args.channel, Path(args.sums).read_text(encoding="utf-8"),
                                       args.out):
                print(path)
        elif args.command == "homebrew-tap":
            update_tap(args.tap, args.formula, args.repo, args.version, args.release_url)
        elif args.command == "build":
            build(args.job, Path(args.out))
        elif args.command == "install":
            install(args.into)
        elif args.command == "vendored":
            if problems := vendored():
                print("\n".join(problems), file=sys.stderr)
                return 1
            manifest = json.loads((root() / MANIFEST).read_text(encoding="utf-8"))
            print(f"release-kit {manifest['version']}: the installed copy is unmodified.")
        elif args.command == "preflight":
            preflight(args.registry, args.selection)
        elif args.command == "verify":
            verify(args.registry, args.selection, timeout=args.timeout)
        elif args.command == "consume":
            consume(args.registry, args.selection)
        elif args.command == "publish-crates":
            # Tokens come from the environment, never the command line.
            publish_crates(args.selection, args.new_crates, os.environ.get("RELEASE_TRUSTED_TOKEN", ""),
                           os.environ.get("RELEASE_NEW_CRATE_TOKEN", ""))
    except (RuntimeError, ValueError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
