"""rab (Release Asset Bundle, formerly release-kit), the buchochelliq-labs release
driver: rab.yml, checked, built, tagged and gated.

rab.yml lists every package a repository publishes, on which registry,
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
    Each channel has a retry policy (RETRY): a registry's version is published
    once; a Homebrew formula or Chocolatey package made from a published GitHub
    release is published again until it is done, read live from the tap or
    the community repository.

Commands (person):
    check       rab.yml matches every manifest, names no package twice,
                lists each package after the listed packages it depends on,
                and declares artifacts each registry can publish; and no
                crates.io package has what `cargo publish` would refuse.
                --offline, --publish-dry-run.
    sync        rewrite rab.yml's versions from the manifests.
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
                refuse, before uploading, any selected package of R already on R
                (and, for crates.io, any crate `check` finds not ready).
    verify --registry R --selection JSON
                wait until every selected package of R is on R.
    pypi-upload --dir DIR
                upload DIR's wheels and sdists to PyPI by Trusted Publishing,
                with PEP 740 attestations (--attestations false: without).
    plan --selection JSON
                the build matrix: one job per artifact (and target) of the
                selected packages. Writes `jobs` and `count` to GITHUB_OUTPUT.
    prepare --job JSON
                run the job's artifact's `prepare` command, before building.
    build --job JSON
                build one planned job into dist/, with a .sha256 per file for
                GitHub Releases.
    github-release --name NAME --stage DIR --notes-out FILE
                shape a github package's release as rab.yml says:
                write its `checksums` file into DIR, its `notes` into FILE,
                and print what the publish action should do (JSON).

    render --name NAME --channel homebrew|chocolatey --sums FILE --out DIR
                render a github package's Homebrew formula or Chocolatey
                package from its templates and its release's checksums.
    chocolatey-refused --name NAME --log FILE
                after a failed `choco push`: succeed when the community
                repository refused it only because it already has the version
                (and has not rejected it), else fail.
    homebrew-tap --tap DIR --formula FILE --repo OWNER/TAP --version V --release-url URL
                put the formula on a branch of the tap and open a pull request.
    record --tag TAG --sha SHA --selection JSON --verify-only B --needs JSON
                the run's release record (release-record.json): the release,
                its commit, the run, and each package's outcome, files and
                registry URL. The workflow's last job writes it, always.

Commands (installing):
    install     from a rab checkout, copy the kit into the repository you
                are in: .github/rab/ (this script and the actions) and
                .github/workflows/rab-build.yml, with a manifest of checksums;
                and a starter release.yml if it has none. A repository with
                the release-kit 1.x layout is migrated to the new names.
    vendored    the installed copy is unmodified (run it in CI): it changes
                only by installing again.

Registries: github (GitHub Releases), crates.io, pypi, npm, vscode (Visual
Studio Marketplace, and Open VSX with `open-vsx: true`). Artifact kinds:
rust-binary, python, maturin, vsix, ci-artifact and command. Requires Python 3.11+, git and PyYAML (for rab.yml); `tag`
follows runs with the GitHub CLI (`gh`) when it is installed.

Names from release-kit 1.x still work until rab 3.0.0, each with a warning:
the config release-kit.yml (when there is no rab.yml), an installed copy at
.github/release-kit/, and RELEASE_KIT_OFFLINE.
"""

import sys

if sys.version_info < (3, 11):
    sys.exit(f"release.py needs Python 3.11 or newer (for tomllib); this is {sys.version.split()[0]}")

import argparse
import base64
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

VERSION = "2.1.1"
CONFIG = "rab.yml"
# release-kit 1.x's name for it: still read, with a warning, until rab 3.0.0.
OLD_CONFIG = "release-kit.yml"
REGISTRIES = ("github", "crates.io", "pypi", "npm", "vscode")
NUMBER = r"(?:0|[1-9][0-9]*)"
PRERELEASE = rf"(?:{NUMBER}|[0-9]*[A-Za-z-][0-9A-Za-z-]*)"
SEMVER = rf"{NUMBER}\.{NUMBER}\.{NUMBER}(?:-{PRERELEASE}(?:\.{PRERELEASE})*)?(?:\+[0-9A-Za-z-]+(?:\.[0-9A-Za-z-]+)*)?"
# PyPI versions follow PEP 440 (1.2.0rc1, 1.2.0.post1), not SemVer.
PEP440 = r"[0-9]+(?:\.[0-9]+)*(?:(?:a|b|rc)[0-9]+)?(?:\.post[0-9]+)?(?:\.dev[0-9]+)?"
USER_AGENT = f"buchochelliq-labs-rab/{VERSION} (https://github.com/buchochelliq-labs/release-asset-bundle)"


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
# rab.yml (release-kit.yml in release-kit 1.x)

_warned = set()


def warn_once(message):
    """Print a warning once per run, as a GitHub annotation in Actions."""
    if message not in _warned:
        _warned.add(message)
        prefix = "::warning::" if os.environ.get("GITHUB_ACTIONS") == "true" else "warning: "
        print(prefix + message, file=sys.stderr, flush=True)


def old_config_warning():
    return (f"{OLD_CONFIG} is release-kit 1.x's name for {CONFIG}; rename it (git mv {OLD_CONFIG} {CONFIG}, "
            f"or install rab again, which migrates it). The old name is read until rab 3.0.0.")


def read_config(read):
    """(name, text) of the release configuration through `read`: rab.yml,
    else release-kit.yml with a warning. Both present is refused: which one
    is meant is a guess. Neither: whatever reading rab.yml raised."""
    found, missing = {}, None
    for name in (CONFIG, OLD_CONFIG):
        try:
            found[name] = read(name)
        except (OSError, KeyError, subprocess.CalledProcessError) as error:
            missing = missing or error
    if len(found) == 2:
        raise ValueError(f"both {CONFIG} and {OLD_CONFIG} exist: keep {CONFIG} (rab 2.x's name) and delete "
                         f"{OLD_CONFIG}")
    if OLD_CONFIG in found:
        warn_once(old_config_warning())
        return OLD_CONFIG, found[OLD_CONFIG]
    if CONFIG in found:
        return CONFIG, found[CONFIG]
    raise missing


def config_path(base=None):
    """The configuration file on disk: rab.yml, else release-kit.yml (with a
    warning, and refused when both exist); rab.yml when there is neither."""
    base = Path(base) if base else root()
    new, old = base / CONFIG, base / OLD_CONFIG
    if new.is_file() and old.is_file():
        raise ValueError(f"both {CONFIG} and {OLD_CONFIG} exist: keep {CONFIG} (rab 2.x's name) and delete "
                         f"{OLD_CONFIG}")
    if old.is_file() and not new.is_file():
        warn_once(old_config_warning())
        return old
    return new


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
    # `vscode` packages only: `open-vsx: true` publishes to Open VSX too, and
    # the version counts as published only once both registries have it.
    open_vsx: object = field(default=False, compare=False, hash=False)

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
    data = parse_config(read_config(read)[1])
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
                                tuple(artifacts), tuple(sorted(options.items())),
                                entry.get("open-vsx", False)))
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


def open_vsx_errors(package):
    """What is wrong with a package's `open-vsx` option, as messages."""
    if package.open_vsx is False:
        return []
    if package.registry != "vscode":
        return [f"{package.name}: open-vsx only applies to a vscode package"]
    if not isinstance(package.open_vsx, bool):
        return [f"{package.name}: open-vsx is true or false"]
    return []


def version_key(version):
    """Numeric parts first, so 0.0.10 sorts after 0.0.9."""
    return tuple(int(part) for part in re.findall(r"\d+", version.split("+")[0])[:3])


# Manifests: how each registry's manifest names its package and version.

def _cargo_workspace(read, path, data):
    """The `[workspace]` table a crate inherits from, as cargo finds it: the
    root `package.workspace` names, else the nearest Cargo.toml at or above the
    crate's that has one. {} if there is none."""
    if "workspace" in data:
        return data["workspace"]
    crate = PurePosixPath(path).parent
    named = data.get("package", {}).get("workspace")
    if named:
        candidates = [PurePosixPath(posixpath.normpath(str(crate / named / "Cargo.toml")))]
    else:
        candidates = [parent / "Cargo.toml" for parent in crate.parents]
    for candidate in candidates:
        name = str(candidate).removeprefix("./")
        try:
            manifest = tomllib.loads(read(name))
        except (FileNotFoundError, subprocess.CalledProcessError):
            continue
        if "workspace" in manifest:
            return manifest["workspace"]
    return {}


def _cargo(read, path):
    data = tomllib.loads(read(path))
    package = data.get("package", {})
    version = package.get("version")
    workspace = None
    if isinstance(version, dict) and version.get("workspace"):
        workspace = _cargo_workspace(read, path, data)
        version = workspace["package"]["version"]
    deps = set()
    tables = [data.get("dependencies", {}), data.get("build-dependencies", {})]
    tables += [t.get("dependencies", {}) for t in data.get("target", {}).values()]
    for table in tables:
        for key, spec in table.items():
            deps.add(spec.get("package", key) if isinstance(spec, dict) else key)
    # Cargo keeps a dev-dependency that has a version in the published
    # manifest (it strips only path-only ones), so crates.io must have it first.
    dev = [data.get("dev-dependencies", {})]
    dev += [t.get("dev-dependencies", {}) for t in data.get("target", {}).values()]
    for table in dev:
        for key, spec in table.items():
            if isinstance(spec, dict) and spec.get("workspace"):
                if workspace is None:
                    workspace = _cargo_workspace(read, path, data)
                base = workspace.get("dependencies", {}).get(key, {})
                versioned = isinstance(base, str) or (isinstance(base, dict) and "version" in base)
                name = base.get("package", key) if isinstance(base, dict) else key
            else:
                versioned = isinstance(spec, str) or (isinstance(spec, dict) and "version" in spec)
                name = spec.get("package", key) if isinstance(spec, dict) else key
            if versioned:
                deps.add(name)
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
    """Every way rab.yml can disagree with the repository, as messages."""
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
        errors.extend(open_vsx_errors(package))
    # Each package after the listed packages of its own registry it depends on.
    position = {(p.registry, p.name): i for i, p in enumerate(release.packages)}
    for package in release.packages:
        for dependency in sorted(deps.get(package.name, ())):
            index = position.get((package.registry, dependency))
            if index is not None and index > position[(package.registry, package.name)]:
                errors.append(f"{package.name} is listed before {dependency}, which it depends on")
    return errors


def sync(read=None, path=None):
    """Rewrite each version in rab.yml (or rab.yml) from its
    manifest, keeping the file's comments and layout. Returns the changes made."""
    path = path or config_path()
    read = read or files_on_disk(path.parent)
    text = path.read_text(encoding="utf-8")

    def reading(name):
        if name in (CONFIG, OLD_CONFIG):
            if name != path.name:
                raise FileNotFoundError(name)
            return text
        return read(name)

    release = load(reading)
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
        # With `open-vsx: true`, Open VSX is part of the publication.
        return on_marketplace(package) and (package.open_vsx is not True or on_open_vsx(package))
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


def on_marketplace(package):
    """Whether the Visual Studio Marketplace lists the extension's version."""
    query = {"filters": [{"criteria": [{"filterType": 7, "value": package.name}]}], "flags": 0x1}
    status, body = http_status(
        "https://marketplace.visualstudio.com/_apis/public/gallery/extensionquery",
        data=json.dumps(query).encode(),
        headers={"Content-Type": "application/json", "Accept": "application/json;api-version=7.2-preview.1"})
    results = json.loads(body or b"{}").get("results", [{}])[0].get("extensions", [])
    return any(v.get("version") == package.version for e in results for v in e.get("versions", []))


def on_open_vsx(package):
    """Whether Open VSX serves the extension's version (<publisher>.<name>)."""
    publisher, _, name = package.name.partition(".")
    url = f"https://open-vsx.org/api/{quote(publisher)}/{quote(name)}/{quote(package.version)}"
    return http_status(url)[0] == 200


# ---------------------------------------------------------------------------
# Artifacts: what each package is built into, declared in rab.yml

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
    # A package selected only to finish the channels made from it builds nothing.
    wanted = {(s["registry"], s["name"]) for s in json.loads(selection) if due_on(s, s["registry"])}
    jobs = []
    for package in release.packages:
        if (package.registry, package.name) not in wanted:
            continue
        base = {"package": package.name, "slug": package.slug, "version": package.version,
                "registry": package.registry, "manifest": package.manifest, "tag": package.tag}
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
    """Shape a `github` package's release from rab.yml: write its
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
        target = out / f"{formula_name(settings)}.rb"
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
                  "(rab, the Release Asset Bundle).")
    print(f"tap pull request for {name} {version}: {url}", flush=True)
    return url


# ---------------------------------------------------------------------------
# Retry policy: which publications can be run again
#
# Defined by rab, not configured: it is what each channel allows.
#   once            a version is permanent: never uploaded again (rule 1).
#   finish          an unfinished one (a draft GitHub release) is completed;
#                   a published one is never touched again.
#   idempotent      publishing again is harmless: done again until done.
#   until-accepted  can be pushed again until the channel has accepted it
#                   (accepted includes "in moderation": pushing again would
#                   only replace the version under review), or rejected it
#                   (a rejected version can never be pushed again).
# The registries say themselves whether a version is published (`published_on`);
# the channels made from a GitHub release have their own state, read live
# (`channel_state`), so `tag` and the gate can select a published package
# again for the channels it still lacks.
RETRY = {"github": "finish", "crates.io": "once", "pypi": "once", "npm": "once", "vscode": "once",
         "homebrew": "idempotent", "chocolatey": "until-accepted"}
# Channel states that count as done. Homebrew: `merged` (the tap's default
# branch has the version), `proposed` (an open pull request from the
# release's branch: merging it is the tap's maintainers' step), `prerelease`
# (prereleases are left out of the tap). Chocolatey: `listed` (approved), and
# `submitted` (in its checks or moderation, which can take days). Anything
# else (`due`) is due, except a terminal state.
DONE = {"merged", "proposed", "prerelease", "listed", "submitted"}
# Channel states that end the version on that channel: neither done nor due,
# and never retried. Chocolatey's `rejected`: the community repository keeps a
# rejected version and refuses every push of it ("Package versions that are
# approved, rejected, or exempted cannot be modified"), so only a new version
# can reach Chocolatey. {state: what to tell the maintainer}.
TERMINAL = {"rejected": "rejected: publish a new version"}


def state_text(state):
    """A channel state as `status` and `tag` print it."""
    return TERMINAL.get(state, state)


def channels_of(package):
    """Every channel `package` publishes to: its registry, then the channels
    made from its GitHub release, in CHANNEL_KEYS order."""
    options = dict(package.github)
    return [package.registry] + [c for c in CHANNEL_KEYS if package.registry == "github" and options.get(c)]


def formula_name(settings):
    """A `homebrew` setting's formula name: `name`, else the template's file
    name without .tmpl and .rb."""
    return settings.get("name") or PurePosixPath(settings["formula"]).name.removesuffix(".tmpl").removesuffix(".rb")


def github_headers():
    token = os.environ.get("GH_TOKEN") or os.environ.get("GITHUB_TOKEN")
    return {"Accept": "application/vnd.github+json", **({"Authorization": f"Bearer {token}"} if token else {})}


def homebrew_state(package, settings, fetch=http_status):
    """`merged`, `proposed`, `prerelease` or `due`: whether the tap's default
    branch has Formula/<name>.rb at this version, else whether a pull request
    from the release's branch `<name>-<version>` is open on the tap."""
    if "-" in package.version:
        return "prerelease"
    tap, name = settings["tap"], formula_name(settings)
    status, body = fetch(f"https://api.github.com/repos/{tap}/contents/Formula/{quote(name)}.rb",
                         headers=github_headers())
    if status == 200:
        content = base64.b64decode(json.loads(body).get("content") or "").decode("utf-8", "replace")
        if version_in(content, package.version):
            return "merged"
    head = f"{tap.split('/')[0]}:{name}-{package.version}"
    status, body = fetch(f"https://api.github.com/repos/{tap}/pulls?state=open&head={quote(head)}",
                         headers=github_headers())
    return "proposed" if status == 200 and json.loads(body) else "due"


def chocolatey_id(directory, commit=None):
    """The `<id>` in a chocolatey package directory's one .nuspec (or
    .nuspec.tmpl), at `commit` or else in the working tree."""
    directory = directory.rstrip("/")
    if commit:
        names = git("ls-tree", "-r", "--name-only", commit, "--", f"{directory}/").splitlines()
        texts = {n: git("show", f"{commit}:{n}") for n in names if n.endswith((".nuspec", ".nuspec.tmpl"))}
    else:
        texts = {str(p): p.read_text(encoding="utf-8") for p in (root() / directory).rglob("*")
                 if p.is_file() and p.name.endswith((".nuspec", ".nuspec.tmpl"))}
    ids = [m[1].strip() for m in (re.search(r"<id>([^<]+)</id>", t) for t in texts.values()) if m]
    if len(ids) != 1 or "${" in ids[0]:
        raise ValueError(f"chocolatey package {directory} needs exactly one .nuspec with a literal <id>")
    return ids[0]


def chocolatey_state(package, settings, fetch=http_status, commit=None):
    """`listed`, `submitted`, `rejected` or `due`, from the community
    repository's API: the version's PackageStatus (Approved or Exempted is
    listed; Submitted, in checks or moderation, is submitted), or due when it
    has never been pushed."""
    ident = chocolatey_id(settings["package"], commit)
    status, body = fetch(f"https://community.chocolatey.org/api/v2/Packages(Id='{quote(ident)}',"
                         f"Version='{quote(package.version)}')")
    if status != 200:
        return "due"
    text = body.decode("utf-8", "replace") if isinstance(body, bytes) else body
    found = re.search(r"<d:PackageStatus>([^<]*)</d:PackageStatus>", text)
    state = found[1].strip().lower() if found else ""
    if state == "rejected":
        return "rejected"
    if state in ("approved", "exempted"):
        return "listed"
    if not state:
        approved = re.search(r"<d:IsApproved[^>]*>([^<]*)</d:IsApproved>", text)
        return "listed" if approved is None or approved[1].strip() == "true" else "submitted"
    return "submitted"


def channel_state(package, channel, commit=None, fetch=http_status):
    """The live state of a channel made from `package`'s GitHub release."""
    settings = dict(dict(package.github).get(channel) or {})
    if channel == "homebrew":
        return homebrew_state(package, settings, fetch)
    if channel == "chocolatey":
        return chocolatey_state(package, settings, fetch, commit)
    raise ValueError(f"{channel} is not a channel made from a release")


# What the community repository answers a push of a version it already has
# and will not replace (approved, exempted or rejected; a version still in its
# checks or moderation is replaced by a new push): NuGetGallery's "A package
# with ID '<id>' and version '<version>' already exists and cannot be
# modified.", which choco 2.x rewrites as "This package version already exists
# on the repository and cannot be modified." (choco's NugetPush.cs). Only this
# phrase: choco's generic "It's possible the package version already exists"
# (any 406 or 409, an invalid nuspec too) is not matched.
CHOCOLATEY_EXISTS = re.compile(r"already exists (?:on the repository )?and cannot be modified", re.I)


def chocolatey_refused(name, log, state_of=None, read=None):
    """After `choco push` failed for package `name` (log: its output): the
    channel's state when the push was refused only because the community
    repository already has the version, which counts as pushed (a re-run
    after a push whose answer was lost, or a version listed since the
    gate looked). Raises otherwise, and when the version was rejected: a
    rejected version cannot be pushed again, so it needs a new version."""
    if not CHOCOLATEY_EXISTS.search(log):
        raise RuntimeError(f"{name}: choco push failed (its output is above)")
    package = next((p for p in load(read or files_on_disk()).packages if p.name == name), None)
    if package is None:
        raise RuntimeError(f"{name} is not a package of {CONFIG}")
    try:
        state = (state_of or channel_state)(package, "chocolatey")
    except (RuntimeError, OSError, ValueError) as error:
        print(f"{package}: cannot read its Chocolatey state ({error})", flush=True)
        state = "unknown"
    if state in TERMINAL:
        raise RuntimeError(f"{package}: Chocolatey {TERMINAL[state]} (a rejected version cannot be pushed "
                           "again: fix the package and release a new version)")
    print(f"{package}: the community repository already has this version ({state}); counted as pushed",
          flush=True)
    return state


def pending_channels(package, state_of, ended=None):
    """{channel: state} of the channels made from a published `package` that
    are not done and may be run again (`idempotent` or `until-accepted`).
    `state_of(package, channel)` reads a channel's state. A channel in a
    TERMINAL state is not pending: it goes into `ended` ({channel: state}),
    when given."""
    pending = {}
    for channel in channels_of(package)[1:]:
        if RETRY[channel] in ("idempotent", "until-accepted"):
            state = state_of(package, channel)
            if state in TERMINAL:
                if ended is not None:
                    ended[channel] = state
            elif state not in DONE:
                pending[channel] = state
    return pending


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


def changes_since(built, tagged):
    """How `tagged` relates to `built` in this repository, from the GitHub API
    (the CI run's commit need not be in the release checkout): the compare
    status (`identical`, `ahead`, `behind` or `diverged`) and the files that
    differ between them."""
    status, _, body = github_api(f"/repos/{repository()}/compare/{built}...{tagged}")
    if status != 200:
        raise RuntimeError(f"cannot compare {built} with {tagged} in {repository()} (HTTP {status})")
    data = json.loads(body)
    return data.get("status"), [entry["filename"] for entry in data.get("files") or []]


def version_in(name, version):
    """Whether a file name carries exactly `version` (optionally as `v1.0`, as
    rust-binary archives do): not `11.0` or `1.0.1` for `1.0`, nor its
    pre-release `1.0rc1` or a local `1.0+x`."""
    return re.search(rf"(?<![0-9A-Za-z.])v?{re.escape(version)}(?![0-9A-Za-z+]|\.[0-9])", name) is not None


def promote(job, out, tagged=None):
    """Copy the files of one pinned CI artifact into `out`, unchanged, after
    proving they are the reviewed bytes of this release: the archive has the
    pinned SHA-256 and has not expired, and the tagged commit is the commit
    that built it, or a descendant that changes nothing but `rab.yml`
    (rule 4: a release is one commit's files). The pin cannot be in the commit
    the artifact was built from, so the commit that records it is allowed."""
    ident, pinned = job["artifact"], job["sha256"]
    where = f"{job['package']}: CI artifact {ident}"
    status, _, body = github_api(f"/repos/{repository()}/actions/artifacts/{ident}")
    if status != 200:
        raise RuntimeError(f"{where}: not found in {repository()} (HTTP {status})")
    meta = json.loads(body)
    if meta.get("expired"):
        raise RuntimeError(f"{where} has expired; pin a newer run's artifact")
    if meta.get("digest") and meta["digest"] != f"sha256:{pinned}":
        raise RuntimeError(f"{where}: GitHub records {meta['digest']}, rab.yml pins sha256:{pinned}")
    built = (meta.get("workflow_run") or {}).get("head_sha")
    if not built:
        raise RuntimeError(f"{where}: GitHub does not say which commit built it")
    tagged = tagged or git("rev-parse", "HEAD")
    relation, changed = changes_since(built, tagged)
    if relation not in ("identical", "ahead"):
        raise RuntimeError(f"{where} was built from {built[:12]}, which the tagged commit does not descend from "
                           f"({relation}): a release is one commit. Pin an artifact built on this branch.")
    if other := [name for name in changed if name not in (CONFIG, OLD_CONFIG)]:
        raise RuntimeError(f"{where} was built from {built[:12]}; since then {', '.join(other[:5])}"
                           f"{' and more' if len(other) > 5 else ''} changed: a release is one commit. "
                           f"Only {CONFIG} (the pin itself) may change after the build.")
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
                           f"rab.yml pins {pinned}")
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
            if not version_in(parts[-1], version):
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
            "Give the package a tag format of its own in rab.yml (for example "
            "`tag: \"python-v{version}\"` for a PyPI package named like a crate), or bump its version.")


def release_at(commit, package=None):
    """The release configured at `commit`; a commit without rab.yml or
    rab.yml (a tag from before the repository adopted the kit) is refused."""
    try:
        return load(files_at(commit))
    except subprocess.CalledProcessError:
        what = f"{package.tag} is on {commit[:12]}, which" if package else f"{commit[:12]}"
        raise RuntimeError(f"{what} has no {CONFIG} (nor {OLD_CONFIG}): the tag predates the kit. Bump the package's "
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


def tag_message(subject, release, sha, carried=None):
    carry = f"Carried-from: {carried}\n" if carried else ""
    return f"{subject}\n\nRelease: {release.name}\nCommit: {sha}\n{carry}"


def trailers(tag_name):
    text = git("for-each-ref", f"refs/tags/{tag_name}", "--format=%(contents)")
    return dict(re.findall(r"(?m)^(Release|Commit|Carried-from): (\S+)$", text))


# ---------------------------------------------------------------------------
# The gate (CI)


def gate(tag_name, remote="origin", status=published_on, output=None, verify_only=False,
         channel_status=channel_state):
    """Everything a release run must hold before building, from the checked-out
    tag. Returns the outputs written to GITHUB_OUTPUT. A published package
    whose release's derived channels (Homebrew, Chocolatey) are not done is
    selected for those channels only: `channel_status(package, channel,
    commit)` reads their state."""
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
    # A release carried from an older commit (`tag --commit`, after the run
    # there stopped): its published packages keep their records there, and
    # their channels are finished here. The split check below proves every
    # package tagged there ships the same files here.
    carried = recorded.get("Carried-from")
    if carried and subprocess.run(["git", "merge-base", "--is-ancestor", carried, sha], cwd=root()).returncode:
        raise RuntimeError(f"{tag_name} is carried from {carried[:12]}, which is not an ancestor of {sha[:12]}")
    unpublished = [p for p in release.packages if not status(p)]
    if foreign := not_its_own(release, existing, sha, remote, unpublished):
        raise RuntimeError(foreign_error(foreign, existing))
    if verify_only:
        return verify_selection(tag_name, release, sha, existing, status, output)
    if split := split_from(release, existing, sha, lambda package, tagged: True):
        raise RuntimeError(split_error(sha, split))
    due, channels = [], {}
    for package in release.packages:
        if not status(package):
            print(f"{package}: not yet published; selected", flush=True)
            due.append(package)
            channels[package] = channels_of(package)
            continue
        pending, ended = {}, {}
        if existing.get(package.tag) in (sha, carried or sha):
            pending = pending_channels(package, lambda p, c: channel_status(p, c, sha), ended)
        for channel, state in ended.items():
            print(f"{package}: {channel} is {TERMINAL[state]}; not selected for it", flush=True)
        if pending:
            print(f"{package}: already published; selected for "
                  + ", ".join(f"{c} ({state})" for c, state in pending.items()), flush=True)
            due.append(package)
            channels[package] = list(pending)
        else:
            print(f"{package}: already published; not selected", flush=True)
    if not due:
        raise RuntimeError(f"{tag_name}: every package of release {release.name} is already published")
    untagged = [f"{p.tag} ({'missing' if p.tag not in existing else 'at ' + existing[p.tag][:12]})"
                for p in due if existing.get(p.tag) != sha
                and not (carried and existing.get(p.tag) == carried and status(p))]
    if untagged:
        raise RuntimeError(f"each selected package's tag must be on {sha[:12]}: {', '.join(untagged)}. "
                           "release.py tag pushes them.")
    if plain := lightweight(remote, [p.tag for p in due]):
        raise RuntimeError(f"record tags must be annotated: {', '.join(sorted(plain))}")
    outputs = gate_outputs(sha, release, due, False, output, channels)
    print(f"{tag_name} -> {sha} (release {release.name}): publishing "
          + ", ".join(f"{p}: {', '.join(channels[p])}" if channels[p] != channels_of(p) else str(p)
                      for p in due), flush=True)
    return outputs


def gate_outputs(sha, release, chosen, verify_only, output=None, channels=None):
    """The gate's outputs. `channels` maps each chosen package to the channels
    due for it (default: all of them); the selection carries them, and each
    registry's or channel's flag is true when a chosen package has it due."""
    channels = channels or {p: channels_of(p) for p in chosen}
    outputs = {
        "sha": sha,
        "release": release.name,
        "selection": json.dumps([{"name": p.name, "registry": p.registry, "version": p.version,
                                  "manifest": p.manifest, "tag": p.tag, "slug": p.slug,
                                  "channels": channels[p],
                                  **({"open-vsx": True} if p.open_vsx is True else {})} for p in chosen]),
        "verify-only": str(verify_only).lower(),
    }
    for registry in REGISTRIES:
        outputs[registry.replace(".", "-")] = str(any(p.registry == registry and registry in channels[p]
                                                      for p in chosen)).lower()
    # Channels made from a GitHub release: whether a chosen package has one due.
    for channel in CHANNEL_KEYS:
        outputs[channel] = str(any(channel in channels[p] for p in chosen)).lower()
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
    selection carries each package's rendered tag, which serves as its format.
    A package selected only for the channels made from its release (its own
    upload is done) is left out."""
    return [Package(s["name"], s["registry"], s["manifest"], s["version"], s.get("tag", "{name}-v{version}"),
                    open_vsx=s.get("open-vsx", False))
            for s in json.loads(selection) if s["registry"] == registry and due_on(s, registry)]


def due_on(entry, channel):
    """Whether a selection entry has `channel` due: one of its `channels`, or,
    in a selection from before channels were selected, any channel."""
    return channel in entry.get("channels", [channel])


def crate_exists(name):
    """Whether crates.io has the crate at all (any version)."""
    return http_status(f"https://crates.io/api/v1/crates/{quote(name)}")[0] == 200


def preflight(registry, selection, status=published_on, exists=crate_exists, output=None, ovsx_token=False,
              ready=None):
    """Refuse, just before uploading, any selected version already published:
    another run may have published it since the gate. For crates.io, also
    say which selected crates are new (their first version needs a token,
    since Trusted Publishing needs the crate to exist): writes `new_crates`
    (JSON), `new_crate` and `existing_crate` to GITHUB_OUTPUT, after `ready`
    (crates_io_readiness, for the selected names) finds nothing that would
    stop `cargo publish`. For vscode, writes `uploads`: which of the two
    registries each extension still lacks."""
    chosen = selected(registry, selection)
    if out := [str(p) for p in chosen if status(p)]:
        raise RuntimeError(f"already on {registry}: {', '.join(out)}; a published version is permanent")
    if registry == "crates.io" and ready and chosen:
        errors, warnings = ready({p.name for p in chosen})
        for warning in warnings:
            print(f"warning: {warning}", flush=True)
        if errors:
            raise RuntimeError("not ready for crates.io:\n  " + "\n  ".join(errors))
    print(f"to publish on {registry}: " + (", ".join(str(p) for p in chosen) or "nothing"), flush=True)
    if registry == "vscode":
        uploads = vscode_uploads(chosen, ovsx_token)
        target = output or os.environ.get("GITHUB_OUTPUT")
        if target:
            with open(target, "a", encoding="utf-8") as handle:
                handle.write(f"uploads={json.dumps(uploads)}\n")
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


def vscode_uploads(chosen, ovsx_token, marketplace=on_marketplace, open_vsx=on_open_vsx):
    """[{name, version, marketplace, open-vsx}]: whether each extension is
    still to be uploaded to each registry. A registry that already has the
    version is skipped, so a re-run uploads only what is missing. An Open VSX
    upload needs its token."""
    uploads = []
    for package in chosen:
        upload = {"name": package.name, "version": package.version, "marketplace": not marketplace(package),
                  "open-vsx": package.open_vsx is True and not open_vsx(package)}
        for registry, label in (("marketplace", "the Marketplace"), ("open-vsx", "Open VSX")):
            if not upload[registry] and (registry == "marketplace" or package.open_vsx is True):
                print(f"{package}: already on {label}; not uploaded there again", flush=True)
        uploads.append(upload)
    missing = [u["name"] for u in uploads if u["open-vsx"]]
    if missing and not ovsx_token:
        raise RuntimeError(f"{', '.join(missing)}: {CONFIG} says open-vsx: true, but no Open VSX token was "
                           "given (the ovsx-token input, from the OVSX_PAT secret)")
    return uploads


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


# PyPI uploads with Trusted Publishing, in the publish-pypi composite action
# itself: pypa/gh-action-pypi-publish runs a Docker image named after the
# calling action's repository and ref, which a composite action does not have
# (it pulled the release's own repository instead, and was denied). This is
# PyPI's documented manual exchange (docs.pypi.org/trusted-publishers/
# using-a-publisher/), then twine, with PEP 740 attestations from
# pypi-attestations, both from publish-pypi/requirements.txt.
PYPI = "https://pypi.org"
UPLOAD_URLS = {"https://pypi.org": "https://upload.pypi.org/legacy/",
               "https://test.pypi.org": "https://test.pypi.org/legacy/"}
CLAIMS = ("sub", "repository", "repository_owner", "repository_owner_id", "workflow_ref", "job_workflow_ref",
          "ref", "environment")
NO_ID_TOKEN = ("GitHub offers this job no OIDC token (ACTIONS_ID_TOKEN_REQUEST_URL is not set): give the "
               "publish-pypi job `permissions: id-token: write`. A pull request from a fork never gets one.")


def http_answer(url, data=None, headers=None, tries=3, sleep=time.sleep):
    """(status, body) of a request, an HTTP error's included (its body says
    why); tried again only when nothing answers."""
    request = Request(url, data=data, headers={"User-Agent": USER_AGENT, **(headers or {})})
    for attempt in range(tries):
        try:
            with urlopen(request, timeout=30) as response:
                return response.status, response.read()
        except HTTPError as error:
            return error.code, error.read()
        except URLError:
            if attempt + 1 < tries:
                sleep(2 ** attempt)
    raise RuntimeError(f"{url.split('?')[0]}: no answer; try again")


def token_claims(jwt):
    """The claims of an OIDC token, as lines for an error message (they say
    what PyPI compared with the trusted publisher)."""
    try:
        payload = jwt.split(".")[1]
        claims = json.loads(base64.urlsafe_b64decode(payload + "=" * (-len(payload) % 4)))
    except (IndexError, ValueError):
        return ["  (the token's claims could not be read)"]
    return [f"  {name}: {claims.get(name, 'MISSING')}" for name in CLAIMS]


def pypi_token(index=PYPI, env=None, fetch=None):
    """A short-lived PyPI API token for this job, by Trusted Publishing: the
    index's OIDC audience, GitHub's OIDC token for it, and the index's
    mint-token exchange. The token is masked in the job's log before it is
    returned."""
    env = os.environ if env is None else env
    fetch = fetch or http_answer
    status, body = fetch(f"{index}/_/oidc/audience")
    if status in (403, 404):
        raise RuntimeError(f"{index} " + ("has Trusted Publishing disabled" if status == 403 else
                                          "does not offer Trusted Publishing") + f" ({status} from /_/oidc/audience)")
    try:
        audience = json.loads(body)["audience"] if status == 200 else None
    except (ValueError, KeyError, TypeError):
        audience = None
    if not audience:
        raise RuntimeError(f"{index}/_/oidc/audience answered {status} without an audience; wait a few minutes "
                           "and try again (https://status.python.org/)")
    request_url, request_token = env.get("ACTIONS_ID_TOKEN_REQUEST_URL"), env.get("ACTIONS_ID_TOKEN_REQUEST_TOKEN")
    if not request_url or not request_token:
        raise RuntimeError(NO_ID_TOKEN)
    status, body = fetch(f"{request_url}{'&' if '?' in request_url else '?'}audience={quote(audience)}",
                         headers={"Authorization": f"bearer {request_token}"})
    try:
        oidc = json.loads(body)["value"] if status == 200 else None
    except (ValueError, KeyError, TypeError):
        oidc = None
    if not oidc:
        raise RuntimeError(f"GitHub refused an OIDC token for audience {audience!r} ({status}); the job needs "
                           "`permissions: id-token: write`")
    status, body = fetch(f"{index}/_/oidc/mint-token", data=json.dumps({"token": oidc}).encode(),
                         headers={"Content-Type": "application/json"})
    try:
        answer = json.loads(body)
    except ValueError:
        raise RuntimeError(f"{index}/_/oidc/mint-token answered an unexpected {status}; wait a few minutes and "
                           "try again (https://status.python.org/)") from None
    if status != 200:
        errors = answer.get("errors") if isinstance(answer, dict) else None
        reasons = [f"  {e.get('code', '?')}: {e.get('description', '')}" for e in errors or [] if isinstance(e, dict)]
        raise RuntimeError("\n".join([
            f"{index} refused the Trusted Publishing exchange ({status}):",
            *(reasons or [f"  {body.decode('utf-8', 'replace')[:500]}"]),
            "The publisher on PyPI must match this job's token (owner, repository, workflow file, environment):",
            *token_claims(oidc),
            "See https://docs.pypi.org/trusted-publishers/troubleshooting/"]))
    token = answer.get("token") if isinstance(answer, dict) else None
    if not token:
        raise RuntimeError(f"{index}/_/oidc/mint-token answered without a token; wait a few minutes and try again")
    print(f"::add-mask::{token}", flush=True)
    return token


def dist_hashes(path):
    """`print-hash`: the digests PyPI shows for a file."""
    data = Path(path).read_bytes()
    return [f"{Path(path).name}", f"  sha256: {hashlib.sha256(data).hexdigest()}",
            f"  md5: {hashlib.md5(data).hexdigest()}",  # noqa: S324 (a digest PyPI shows, not security)
            f"  blake2_256: {hashlib.blake2b(data, digest_size=32).hexdigest()}"]


def pypi_upload(directory, index=PYPI, attestations=True, env=None, fetch=None, runner=None, python=None):
    """Upload every wheel and sdist in `directory` to `index` with Trusted
    Publishing: `twine check`, a minted token (pypi_token), PEP 740
    attestations (`pypi-attestations sign`, with the job's OIDC identity;
    PyPI and TestPyPI only), each file's digests, then `twine upload
    --attestations`. Not --skip-existing: preflight refused anything already
    published. Returns the files uploaded."""
    runner = runner or run
    python = python or sys.executable
    dists = sorted(p for p in Path(directory).iterdir() if p.name.endswith((".whl", ".tar.gz")))
    if not dists:
        raise RuntimeError(f"no wheels or sdists in {directory}")
    if index not in UPLOAD_URLS:
        raise RuntimeError(f"{index}: rab uploads to {' or '.join(UPLOAD_URLS)}")
    runner([python, "-m", "twine", "check", *map(str, dists)])
    token = pypi_token(index, env, fetch)
    files = list(dists)
    if attestations:
        if signed := [d.name for d in dists if Path(f"{d}.publish.attestation").exists()]:
            raise RuntimeError(f"already attested, from an earlier step: {', '.join(signed)}")
        print("::notice::Signing PEP 740 attestations with this job's identity (Sigstore)", flush=True)
        runner([python, "-m", "pypi_attestations", "sign", *map(str, dists)])
        files = [f for d in dists for f in (d, Path(f"{d}.publish.attestation"))]
    for dist in dists:
        print("\n".join(dist_hashes(dist)), flush=True)
    runner([python, "-m", "twine", "upload", "--verbose", "--disable-progress-bar", "--non-interactive",
            *(["--attestations"] if attestations else []), *map(str, files)],
           env={"TWINE_USERNAME": "__token__", "TWINE_PASSWORD": token, "TWINE_REPOSITORY_URL": UPLOAD_URLS[index]})
    return files


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
        with tempfile.TemporaryDirectory(prefix="rab-consume-") as scratch:
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
# The release record: one JSON file per run, for coordinators

# Its schema, versioned: `schema_version` changes only when a field is removed,
# renamed or changes meaning; new fields may appear without it, so readers
# ignore what they do not know. docs/release-record.schema.json is this, as
# `release.py record --schema` prints it (a test keeps them equal).
RECORD_VERSION = 1
OUTCOMES = ("published", "already-published", "failed", "skipped", "unknown")
RESULTS = ("complete", "partial", "nothing-published", "unknown")
JOB_RESULTS = ("success", "failure", "cancelled", "skipped")
# The release.yml job that publishes each registry, and those that make a
# github package's channels.
PUBLISH_JOBS = {"github": "publish-github", "crates.io": "publish-crates", "pypi": "publish-pypi",
                "npm": "publish-npm", "vscode": "publish-vscode"}
CHANNEL_JOBS = {"homebrew": "publish-homebrew", "chocolatey": "publish-chocolatey"}
RECORD_SCHEMA = {
    "$schema": "https://json-schema.org/draft/2020-12/schema",
    "$id": "https://buchochelliq-labs.github.io/release-asset-bundle/release-record.schema.json",
    "title": "rab release record",
    "description": "What one release run set out to publish, and what came of it.",
    "type": "object",
    "required": ["schema_version", "repository", "release", "tag", "commit", "verify_only", "run",
                 "result", "jobs", "packages"],
    "properties": {
        "schema_version": {"const": RECORD_VERSION},
        "repository": {"type": "string", "description": "owner/name"},
        "release": {"type": ["string", "null"],
                    "description": "the release's name; null if rab.yml could not be read"},
        "tag": {"type": "string", "description": "the release tag the run was for"},
        "commit": {"type": ["string", "null"], "pattern": "^[0-9a-f]{40}$",
                   "description": "the tagged commit; null if the tag could not be resolved"},
        "verify_only": {"type": "boolean"},
        "run": {
            "type": "object",
            "required": ["id", "attempt", "url"],
            "properties": {
                "id": {"type": ["integer", "null"]},
                "attempt": {"type": ["integer", "null"]},
                "url": {"type": ["string", "null"]},
            },
        },
        "result": {"enum": list(RESULTS),
                   "description": "complete: every selected package is on its registry, its publish "
                                  "job and the jobs of the channels due for it succeeded; partial: this run published some of them; "
                                  "nothing-published: this run published none; unknown: a registry did "
                                  "not answer"},
        "jobs": {"type": "object", "description": "the result of each job the record job needs",
                 "additionalProperties": {"enum": list(JOB_RESULTS)}},
        "packages": {
            "type": "array",
            "description": "every package of rab.yml at the tagged commit, in its order",
            "items": {
                "type": "object",
                "required": ["name", "registry", "version", "tag", "selected", "outcome", "job", "url",
                             "files"],
                "properties": {
                    "name": {"type": "string"},
                    "registry": {"enum": list(REGISTRIES)},
                    "version": {"type": "string"},
                    "tag": {"type": "string", "description": "its record tag"},
                    "selected": {"type": "boolean", "description": "whether the gate selected it"},
                    "outcome": {"enum": list(OUTCOMES)},
                    "job": {"enum": [*JOB_RESULTS, None],
                            "description": "its publish job's result; null when not selected, or "
                                           "selected only to finish its channels"},
                    "url": {"type": ["string", "null"],
                            "description": "where its registry serves this version; null when it does not"},
                    "files": {
                        "type": "array",
                        "description": "the files this run built for it, from the build artifacts",
                        "items": {
                            "type": "object",
                            "required": ["name", "sha256", "size"],
                            "properties": {
                                "name": {"type": "string"},
                                "sha256": {"type": "string", "pattern": "^[0-9a-f]{64}$"},
                                "size": {"type": "integer", "minimum": 0},
                            },
                        },
                    },
                    "open_vsx_url": {"type": ["string", "null"],
                                     "description": "a vscode package with open-vsx: true: where Open VSX "
                                                    "serves this version; null when it does not"},
                    "channels": {"type": "object",
                                 "description": "a selected github package's homebrew and chocolatey "
                                                "jobs, those due for it in this run: their results",
                                 "additionalProperties": {"enum": list(JOB_RESULTS)}},
                },
            },
        },
    },
}
_JSON_TYPES = {"object": dict, "array": list, "string": str, "boolean": bool, "null": type(None)}


def _is_type(value, kind):
    if kind == "integer":
        return isinstance(value, int) and not isinstance(value, bool)
    return isinstance(value, _JSON_TYPES[kind]) and (kind == "boolean" or not isinstance(value, bool))


def schema_errors(value, schema, where="record"):
    """What in `value` breaks `schema`, as messages: the JSON Schema keywords
    RECORD_SCHEMA uses (type, const, enum, pattern, minimum, required,
    properties, additionalProperties, items), so release.py needs no library."""
    kinds = schema["type"] if isinstance(schema.get("type"), list) else [schema["type"]] if "type" in schema else []
    if kinds and not any(_is_type(value, kind) for kind in kinds):
        return [f"{where}: {value!r} is not {' or '.join(kinds)}"]
    errors = []
    if "const" in schema and value != schema["const"]:
        errors.append(f"{where}: {value!r} is not {schema['const']!r}")
    if "enum" in schema and value not in schema["enum"]:
        errors.append(f"{where}: {value!r} is not one of {', '.join(map(str, schema['enum']))}")
    if "pattern" in schema and isinstance(value, str) and not re.search(schema["pattern"], value):
        errors.append(f"{where}: {value!r} does not match {schema['pattern']}")
    if "minimum" in schema and _is_type(value, "integer") and value < schema["minimum"]:
        errors.append(f"{where}: {value} is below {schema['minimum']}")
    if isinstance(value, dict):
        errors += [f"{where}: {key} is missing" for key in schema.get("required", []) if key not in value]
        for key, item in value.items():
            if key in schema.get("properties", {}):
                errors += schema_errors(item, schema["properties"][key], f"{where}.{key}")
            elif isinstance(schema.get("additionalProperties"), dict):
                errors += schema_errors(item, schema["additionalProperties"], f"{where}.{key}")
    if isinstance(value, list) and "items" in schema:
        for index, item in enumerate(value):
            errors += schema_errors(item, schema["items"], f"{where}[{index}]")
    return errors


def registry_url(package, repo):
    """The page where `package`'s registry serves its version."""
    name, version = package.name, package.version
    return {
        "github": f"https://github.com/{repo}/releases/tag/{quote(package.tag)}",
        "crates.io": f"https://crates.io/crates/{quote(name)}/{quote(version)}",
        "pypi": f"https://pypi.org/project/{quote(name)}/{quote(pep440(version))}/",
        "npm": f"https://www.npmjs.com/package/{quote(name, safe='@/')}/v/{quote(version)}",
        "vscode": f"https://marketplace.visualstudio.com/items?itemName={quote(name)}",
    }[package.registry]


def open_vsx_url(package):
    """The page where Open VSX serves an extension's version."""
    publisher, _, name = package.name.partition(".")
    return f"https://open-vsx.org/extension/{quote(publisher)}/{quote(name)}/{quote(package.version)}"


def built_files(dist, slug):
    """[{name, sha256, size}] of the files this run built for a package: every
    file in `<dist>/release--<slug>--*/` but the `.sha256` ones, each checked
    against its `.sha256` where the build wrote one (a github package's)."""
    pattern = re.sub(r"([*?\[])", r"[\1]", f"release--{slug}--") + "*/*"
    files = []
    for path in Path(dist).glob(pattern):
        if not path.is_file() or path.name.endswith(".sha256"):
            continue
        sha = hashlib.sha256(path.read_bytes()).hexdigest()
        sums = Path(f"{path}.sha256")
        if sums.exists() and parse_sums(sums.read_text(encoding="utf-8")).get(path.name) != sha:
            raise RuntimeError(f"{path}: its .sha256 does not match the file (SHA-256 {sha})")
        files.append({"name": path.name, "sha256": sha, "size": path.stat().st_size})
    return sorted(files, key=lambda f: (f["name"], f["sha256"]))


def job_results(needs):
    """{job: result} from a job's `toJSON(needs)` ({job: {result, outputs}})."""
    needs = json.loads(needs or "{}") if isinstance(needs, str) else needs
    return {job: value.get("result") if isinstance(value, dict) else value for job, value in needs.items()}


def run_info(repo):
    """The run this is, from the Actions environment (nulls outside it)."""
    ident, attempt = os.environ.get("GITHUB_RUN_ID", ""), os.environ.get("GITHUB_RUN_ATTEMPT", "")
    server = os.environ.get("GITHUB_SERVER_URL", "https://github.com")
    return {"id": int(ident) if ident.isdigit() else None,
            "attempt": int(attempt) if attempt.isdigit() else None,
            "url": f"{server}/{repo}/actions/runs/{ident}" if ident.isdigit() else None}


def record(tag_name, sha, selection, verify_only, needs, dist="dist", read=None, status=published_on,
           repo=None, run=None):
    """The release record of one run (RECORD_SCHEMA): the release, its commit,
    the run, its jobs' results, and every package of rab.yml with
    what came of it. The release workflow's last job writes it whatever
    happened before, so a coordinator can tell a run that published nothing
    from one that published part of the release. `needs` is that job's
    `toJSON(needs)`; `selection` and `sha` the gate's outputs (empty when
    the gate failed). A selected package whose publish job did not succeed
    is looked up on its registry: a job can fail after uploading. A package
    the gate selected only to finish the channels made from its release
    (its entry's `channels` leave out its registry) was already published:
    its record is those channel jobs' results."""
    jobs = job_results(needs)
    repo = repo or repository()
    gated = jobs.get("gate") == "success"
    chosen = {(s["registry"], s["name"]): s for s in json.loads(selection or "[]")} if gated else {}
    if not sha:
        try:
            sha = git("rev-parse", "--verify", f"refs/tags/{tag_name}^{{commit}}")
        except subprocess.CalledProcessError:
            sha = None
    try:
        release = load(read or files_at(sha)) if read or sha else None
    except (ValueError, RuntimeError, subprocess.CalledProcessError, FileNotFoundError) as error:
        print(f"cannot read {CONFIG} at {sha}: {error}", flush=True)
        release = None
    on_registry = "already-published" if verify_only else "published"
    packages, done, published_any = [], [], False
    for package in release.packages if release else ():
        selected = (package.registry, package.name) in chosen
        job, files, channels = None, [], {}
        if selected:
            entry = chosen[package.registry, package.name]
            # Its due channels made from a release (all of them, in a selection from before #20).
            channels = {channel: jobs.get(CHANNEL_JOBS[channel]) or "skipped"
                        for channel in entry.get("channels", channels_of(package)) if channel in CHANNEL_JOBS}
        if not selected:
            # A normal run's gate selects every package that is not yet published.
            outcome = "already-published" if gated and not verify_only else "skipped"
        elif not due_on(entry, package.registry):
            # Selected only to finish its channels: the gate saw it published.
            outcome = "already-published"
        else:
            name = PUBLISH_JOBS[package.registry]
            if name not in jobs:
                raise RuntimeError(f"the record job does not need {name}, which publishes {package}: "
                                   "add it to the job's `needs`")
            job = jobs[name]
            if job == "success":
                outcome = on_registry
            else:
                try:
                    there = status(package)
                except (RuntimeError, OSError, ValueError) as error:
                    print(f"{package}: cannot tell whether it is published: {error}", flush=True)
                    there = None
                outcome = ("unknown" if there is None else on_registry if there
                           else "skipped" if job == "skipped" else "failed")
            files = built_files(dist, package.slug)
        if selected:
            # Done: its own upload (when due) and, outside a verify-only run
            # (which runs no channel job), every channel due for it.
            own = job is None or (outcome == on_registry and job == "success")
            done.append(own and (verify_only or all(r == "success" for r in channels.values())))
            published_any |= outcome == "published" or (
                not verify_only and any(r == "success" for r in channels.values()))
        entry = {"name": package.name, "registry": package.registry, "version": package.version,
                 "tag": package.tag, "selected": selected, "outcome": outcome, "job": job,
                 "url": registry_url(package, repo) if outcome in ("published", "already-published") else None,
                 "files": files}
        if package.registry == "vscode" and package.open_vsx is True:
            # Published means on both registries (published_on), so both pages serve it.
            entry["open_vsx_url"] = open_vsx_url(package) if entry["url"] else None
        if channels:
            entry["channels"] = channels
        packages.append(entry)
    mine = [p for p in packages if p["selected"]]
    if mine and all(done):
        result = "complete"
    elif published_any:
        result = "partial"
    elif any(p["outcome"] == "unknown" for p in mine):
        result = "unknown"
    else:
        result = "nothing-published"
    document = {"schema_version": RECORD_VERSION, "repository": repo,
                "release": release.name if release else None, "tag": tag_name, "commit": sha or None,
                "verify_only": bool(verify_only), "run": run or run_info(repo), "result": result,
                "jobs": jobs, "packages": packages}
    if errors := schema_errors(document, RECORD_SCHEMA):
        raise RuntimeError("the release record breaks its schema:\n  " + "\n  ".join(errors))
    return document


def record_lines(document):
    """The record for a person: one line for the run, one per package."""
    lines = [f"release {document['release']} ({document['tag']}): {document['result']}"]
    for package in document["packages"]:
        job = f" (publish job: {package['job']})" if package["job"] else ""
        if package.get("channels"):
            job += " (" + ", ".join(f"{c}: {r}" for c, r in package["channels"].items()) + ")"
        lines.append(f"  {package['name']} {package['version']} on {package['registry']}: "
                     f"{package['outcome']}{job}")
    return lines


# ---------------------------------------------------------------------------
# crates.io readiness: what would stop `cargo publish` after the tag, found
# by `check` (and again by `preflight --registry crates.io`) before it.
# Manifests are read with `cargo metadata`, so workspace inheritance and
# renamed dependencies mean what cargo says they mean.

OFFLINE = "RAB_OFFLINE"
OLD_OFFLINE = "RELEASE_KIT_OFFLINE"  # release-kit 1.x's name: still read, with a warning
LIBRARY_KINDS = {"lib", "rlib", "dylib", "proc-macro"}
# include_str!("..."), include_bytes!("...") and include!("..."), relative to
# the file, or concat!(env!("CARGO_MANIFEST_DIR"), "/..."), relative to the crate.
EMBED = re.compile(r'\binclude(?:_str|_bytes)?!\s*\(\s*(concat!\s*\(\s*env!\s*\(\s*"CARGO_MANIFEST_DIR"\s*\)\s*,\s*)?'
                   r'"([^"]+)"')
# Packaged files that `cargo publish` does not build: their embeds cannot fail it.
NOT_BUILT = ("tests/", "benches/", "examples/")
# Nor does it build test code: an item under #[cfg(test)] or #[test].
TEST_ITEM = re.compile(r"#\[\s*(?:test\s*\]|cfg\s*\(\s*(?:all\s*\(\s*)?test\b)")


def offline():
    """Whether checks that ask crates.io are skipped (RAB_OFFLINE=1; release-kit
    1.x's RELEASE_KIT_OFFLINE is read too, with a warning, when RAB_OFFLINE is unset)."""
    value = os.environ.get(OFFLINE)
    if value is None and (value := os.environ.get(OLD_OFFLINE)) is not None:
        warn_once(f"{OLD_OFFLINE} is release-kit 1.x's name: set {OFFLINE} instead (the old name is read "
                  "until rab 3.0.0)")
    return (value or "").strip().lower() in {"1", "true", "yes"}


_last_api_call = [0.0]


def polite_fetch(url):
    """http_status, at most one crates.io API request a second (its data
    access policy); the sparse index is a CDN and needs no spacing."""
    if url.startswith("https://crates.io/api/"):
        time.sleep(max(0.0, _last_api_call[0] + 1 - time.monotonic()))
        _last_api_call[0] = time.monotonic()
    return http_status(url)


def cargo_run(args, cwd):
    return subprocess.run(["cargo", *args], cwd=cwd, text=True, encoding="utf-8", capture_output=True)


def cargo_failure(result):
    lines = [line for line in (result.stderr or "").splitlines() if line.strip()]
    errors = [line for line in lines if line.startswith("error")]
    return (errors[0] if errors else lines[-1] if lines else f"exit {result.returncode}").strip()


def index_url(name):
    """The crate's file in crates.io's sparse index: every version, one JSON
    object per line."""
    name = name.lower()
    prefix = {1: "1", 2: "2", 3: f"3/{name[:1]}"}.get(len(name), f"{name[:2]}/{name[2:4]}")
    return f"https://index.crates.io/{prefix}/{quote(name)}"


def crate_versions(name, fetch):
    """The versions of `name` on crates.io that are not yanked, or None if
    there is no such crate."""
    status, body = fetch(index_url(name))
    if status == 404:
        return None
    entries = [json.loads(line) for line in body.decode("utf-8").splitlines() if line.strip()]
    return [e["vers"] for e in entries if not e.get("yanked") and e.get("name", name).lower() == name.lower()]


def _semver(text):
    """(major, minor, patch, pre) from a version, or a partial one (missing
    parts are None); pre is a tuple of identifiers."""
    match = re.fullmatch(r"(\d+|[*xX])(?:\.(\d+|[*xX]))?(?:\.(\d+|[*xX]))?(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?",
                         text.strip())
    if not match:
        raise ValueError(f"not a version: {text!r}")
    parts = [None if p is None or p in "*xX" else int(p) for p in match.groups()[:3]]
    pre = tuple(match[4].split(".")) if match[4] else ()
    return (*parts, pre)


def _order(major, minor, patch, pre):
    """A sort key in SemVer precedence: a pre-release sorts before its release."""
    ids = tuple((0, int(i), "") if i.isdigit() else (1, 0, i) for i in pre)
    return (major, minor, patch, (0, ids) if pre else (1, ()))


def _comparator(text):
    """One comparator of a Cargo version requirement: its bounds (low,
    low_inclusive, high, high_inclusive) over `_order` keys (None for
    unbounded), and the (major, minor, patch) of the pre-release it names."""
    match = re.fullmatch(r"\s*(\^|~|=|>=|<=|>|<)?\s*(.+?)\s*", text)
    op, (major, minor, patch, pre) = match[1] or "^", _semver(match[2])
    if re.search(r"[*xX]", match[2]):
        op = "="   # 1.*, 1.2.x: like =1, =1.2
    if major is None:
        return (None, True, None, False), None
    low = _order(major, minor or 0, patch or 0, pre)
    named = (major, minor, patch) if pre else None

    def up(*version):
        return _order(*version, ())

    # The first version past a partial one (1 -> 2.0.0, 1.2 -> 1.3.0).
    past = up(major + 1, 0, 0) if minor is None else up(major, minor + 1, 0) if patch is None else None
    if op == "^":
        if past is None:
            past = up(major + 1, 0, 0) if major else up(0, minor + 1, 0) if minor else up(0, 0, patch + 1)
        elif minor is not None and major:
            past = up(major + 1, 0, 0)
        return (low, True, past, False), named
    if op == "~":
        return (low, True, past or up(major, minor + 1, 0), False), named
    if op == "=":
        return ((low, True, past, False) if past else (low, True, low, True)), named
    if op == ">=":
        return (low, True, None, False), named
    if op == ">":
        return ((past, True, None, False) if past else (low, False, None, False)), named
    if op == "<":
        return (None, True, low, False), named
    return ((None, True, past, False) if past else (None, True, low, True)), named   # <=


def requirement_matches(requirement, version):
    """Whether `version` satisfies a Cargo version requirement (`^1.2`,
    `~0.3.1`, `>=1, <2`, `=1.0.0`, `1.*`, ...). A pre-release matches only a
    requirement that names a pre-release of the same major.minor.patch, as
    in Cargo."""
    major, minor, patch, pre = _semver(version)
    key = _order(major, minor, patch, pre)
    allowed = not pre
    for text in requirement.split(","):
        (low, low_inclusive, high, high_inclusive), named = _comparator(text)
        allowed = allowed or named == (major, minor, patch)
        if low is not None and (key < low or (key == low and not low_inclusive)):
            return False
        if high is not None and (key > high or (key == high and not high_inclusive)):
            return False
    return allowed


def repository_of(url):
    """(host, owner, name) of a repository URL, lower-cased, or None."""
    match = re.match(r"^(?:[a-z+]+://)?(?:[^@/]+@)?([^/:]+)[/:]([^/]+)/([^/#?]+?)(?:\.git)?/?(?:[#?].*)?$",
                     (url or "").strip(), re.I)
    return tuple(part.lower() for part in match.groups()) if match else None


def owner_problem(name, repo, fetch):
    """Whether this repository can publish `name` on crates.io, judged from
    the public API: (None, None) when the name is free or ours, ("error",
    message) when it belongs to someone else, ("warning", message) when it
    cannot be told. The repository's owner (a GitHub user or organisation)
    is ours when crates.io lists that user, or a team of that organisation
    (github:<org>:<team>), among the crate's owners, or when the crate's
    `repository` is on GitHub under that owner (a crate an organisation
    member owns personally)."""
    status, body = fetch(f"https://crates.io/api/v1/crates/{quote(name)}")
    if status == 404:
        return None, None
    crate = json.loads(body).get("crate", {})
    if crate.get("name", name) != name:
        return "error", (f"{name}: crates.io has the crate {crate['name']}, and names that differ only in case, "
                         f"`-` or `_` are the same name; rename the package (e.g. {name}-cli, keeping the command "
                         f"with [[bin]] name = \"{name}\")")
    owner = repo.split("/")[0].lower() if repo else None
    declared = repository_of(crate.get("repository"))
    if owner and declared and declared[0] == "github.com" and declared[1] == owner:
        return None, None
    _, body = fetch(f"https://crates.io/api/v1/crates/{quote(name)}/owners")
    users = json.loads(body or b"{}").get("users", [])
    logins = ", ".join(u.get("login", "?") for u in users) or "nobody listed"
    if owner and any(u.get("login", "").lower() == owner or u.get("login", "").lower().startswith(f"github:{owner}:")
                     for u in users):
        return None, None
    if owner and declared and declared[0] == "github.com":
        return "error", (f"{name}: the name is taken on crates.io (owners: {logins}; repository "
                         f"{crate.get('repository')}), and {repo} cannot publish it. Rename the package "
                         f"(e.g. {name}-cli, keeping the command with [[bin]] name = \"{name}\"), or, if the crate "
                         f"is yours, have an owner add {owner} (cargo owner --add {owner}, or "
                         f"github:{owner}:<team> for an organisation)")
    return "warning", (f"{name}: crates.io's {name} is owned by {logins}"
                       + (f" (repository {crate['repository']})" if crate.get("repository") else "")
                       + f"; rab cannot tell whether {repo or 'this repository'} may publish it. Make sure "
                       "its Trusted Publisher names this repository, or that the token's account is an owner")


def without_tests(source):
    """Rust source without its test items (#[cfg(test)] or #[test], up to
    the end of the braced block that follows), which `cargo publish` does
    not compile. Braces are counted, not parsed: good enough for embeds."""
    kept, at = [], 0
    while match := TEST_ITEM.search(source, at):
        kept.append(source[at:match.start()])
        brace, semicolon = source.find("{", match.end()), source.find(";", match.end())
        if brace < 0 or 0 <= semicolon < brace:
            at = match.end()
            continue
        depth, at = 0, brace
        while at < len(source):
            depth += {"{": 1, "}": -1}.get(source[at], 0)
            at += 1
            if depth == 0:
                break
    return "".join(kept) + source[at:]


def embed_problems(name, crate_dir, packaged):
    """Files the crate's sources embed (include_str!, include_bytes!,
    include!) that its package does not carry: a package holds only its
    crate directory, so an embed from elsewhere fails `cargo publish`."""
    problems, carried = [], set(packaged)
    for file in sorted(packaged):
        if not file.endswith(".rs") or file.startswith(NOT_BUILT):
            continue
        try:
            text = (crate_dir / file).read_text(encoding="utf-8")
        except (OSError, UnicodeDecodeError):
            continue
        for line in without_tests(text).splitlines():
            if line.lstrip().startswith("//"):
                continue
            for manifest_relative, literal in EMBED.findall(line):
                start = "" if manifest_relative else posixpath.dirname(file)
                resolved = posixpath.normpath(posixpath.join(start, literal.lstrip("/") if manifest_relative
                                                             else literal))
                if resolved == ".." or resolved.startswith("../"):
                    problems.append(f"{name}: {file} embeds {literal}, outside the crate; a package carries only "
                                    "its crate directory. Copy the file into the crate (and add a test that the "
                                    "copy matches the original)")
                elif resolved not in carried:
                    problems.append(f"{name}: {file} embeds {literal}, which the package does not carry; add it "
                                    "to `include` in Cargo.toml (or take it out of `exclude`)")
    return problems


def _workspaces(release, base, cargo, offline_mode):
    """{manifest path (resolved): (metadata package, workspace metadata)}
    for every Cargo.toml rab.yml names, plus the root workspace's
    members; and the error each unreadable manifest gave."""
    found, errors, seen = {}, {}, set()
    manifests = [p.manifest for p in release.packages if PurePosixPath(p.manifest).name == "Cargo.toml"]
    if (base / "Cargo.toml").is_file():
        manifests.append("Cargo.toml")
    for manifest in manifests:
        path = (base / manifest).resolve()
        if path in found or path in errors:
            continue
        args = ["metadata", "--format-version", "1", "--no-deps", "--manifest-path", str(path)]
        result = cargo(args + (["--offline"] if offline_mode else []), base)
        if result.returncode:
            errors[path] = cargo_failure(result)
            continue
        metadata = json.loads(result.stdout)
        if metadata["workspace_root"] in seen:
            continue
        seen.add(metadata["workspace_root"])
        for package in metadata["packages"]:
            found[Path(package["manifest_path"]).resolve()] = (package, metadata)
    return found, errors


def declared_crates(release, base, found):
    """The crates rab.yml publishes in some form: crates.io packages,
    any package whose manifest is a crate's, the crates maturin builds and
    the crates a rust-binary names."""
    names = {p.name for p in release.packages if p.registry == "crates.io"}
    for package in release.packages:
        manifests = [package.manifest]
        if PurePosixPath(package.manifest).name == "pyproject.toml":
            try:
                manifests.append(maturin_manifest(files_on_disk(base), package.manifest)[0])
            except (OSError, ValueError, tomllib.TOMLDecodeError):
                pass
        for manifest in manifests:
            if hit := found.get((base / manifest).resolve()):
                names.add(hit[0]["name"])
        names.update(a["crate"] for a in package.artifacts if a.get("kind") == "rust-binary" and a.get("crate"))
    return names


def crates_io_readiness(release, base, only=None, fetch=None, cargo=None, repo=None, offline_mode=None):
    """What would stop `cargo publish` for rab.yml's crates.io
    packages (or the ones named in `only`), as (errors, warnings):

    - the manifest says `publish = false`, or lists only other registries;
    - a dependency (normal or build) by `git` or `path` has no `version`, or
      comes from another registry;
    - a dependency that publishing turns into a crates.io one (a `git` or
      `path` dependency with a `version`) is not on crates.io at a matching
      version, and is not a crates.io package listed before this one;
    - the name is on crates.io and belongs to someone else (a warning when
      that cannot be told);
    - `cargo package --list` fails, or a source file embeds a file the
      package does not carry.

    With every package (no `only`), it also warns about the workspace's
    library crates rab.yml does not publish. Checks that ask
    crates.io are skipped offline (RAB_OFFLINE=1)."""
    offline_mode = offline() if offline_mode is None else offline_mode
    fetch = fetch or polite_fetch
    if cargo is None:
        if not shutil.which("cargo"):
            return [], ["cargo is not installed: the crates.io checks (manifests, dependencies, packaging) "
                        "were skipped"]
        cargo = cargo_run
    if repo is None:
        try:
            repo = repository()
        except (RuntimeError, subprocess.CalledProcessError):
            repo = ""
    errors, warnings = [], []
    if not repo and not offline_mode:
        warnings.append("cannot tell this repository's GitHub owner (no GITHUB_REPOSITORY, and origin is not on "
                        "GitHub): who owns each crate on crates.io was not checked")
    found, unreadable = _workspaces(release, base, cargo, offline_mode)
    crates = [p for p in release.packages if p.registry == "crates.io"]
    position = {p.name: i for i, p in enumerate(crates)}
    for package in crates:
        if only is not None and package.name not in only:
            continue
        path = (base / package.manifest).resolve()
        if path in unreadable:
            errors.append(f"{package.name}: cargo metadata cannot read {package.manifest}: {unreadable[path]}")
            continue
        if path not in found:
            continue   # `check` already says the manifest is missing
        meta, _ = found[path]
        problems = []
        publish = meta.get("publish")
        if publish == []:
            problems.append(f"{package.name}: {package.manifest} says `publish = false`, so cargo will not publish "
                            f"it; remove that line (or take the package out of {CONFIG} to keep the crate private)")
        elif publish is not None and "crates-io" not in publish:
            problems.append(f"{package.name}: {package.manifest} publishes only to {', '.join(publish)} "
                            f"(`publish = [...]`); add \"crates-io\" to the list, or remove `publish`")
        crate_dir = path.parent
        checked = set()
        for dep in meta["dependencies"]:
            if dep.get("kind") == "dev" or dep["name"] in checked:
                continue
            checked.add(dep["name"])
            source, local, requirement = dep.get("source") or "", dep.get("path"), dep.get("req") or "*"
            what = f"{dep['name']} (renamed {dep['rename']})" if dep.get("rename") else dep["name"]
            if dep.get("registry"):
                problems.append(f"{package.name}: depends on {what} from the registry {dep['registry']}; crates.io "
                                "accepts only dependencies on crates.io")
                continue
            if source.startswith("git+"):
                if requirement == "*":
                    problems.append(f"{package.name}: depends on {what} from git ({source[4:]}) with no version, "
                                    f"and crates.io refuses git dependencies. Publish {dep['name']} to crates.io "
                                    "first, then give the dependency a `version` (cargo builds from git locally "
                                    "and from crates.io once published)")
                    continue
            elif local:
                if requirement == "*":
                    target = found.get((Path(local) / "Cargo.toml").resolve())
                    version = target[0]["version"] if target else "X.Y.Z"
                    relative = Path(os.path.relpath(local, crate_dir)).as_posix()
                    problems.append(f"{package.name}: depends on {what} by path with no version; crates.io needs "
                                    f"one: {dep.get('rename') or dep['name']} = {{ path = \"{relative}\", "
                                    f"version = \"{version}\" }}")
                    continue
            else:
                continue   # a crates.io dependency, resolved by the lockfile
            # Published, a git or path dependency becomes a crates.io one.
            if dep["name"] in position:
                continue   # a package of this release: `check` orders it before this one
            if offline_mode:
                continue
            versions = crate_versions(dep["name"], fetch)
            fix = (f"publish {dep['name']} first (from its own repository), or list it in {CONFIG} "
                   f"(registry: crates.io) before {package.name}")
            if versions is None:
                problems.append(f"{package.name}: depends on {what} {requirement}, which is not on crates.io; {fix}")
            elif not any(requirement_matches(requirement, v) for v in versions):
                newest = max(versions, key=lambda v: _order(*_semver(v))) if versions else "none"
                problems.append(f"{package.name}: depends on {what} {requirement}, but crates.io has no matching "
                                f"version (newest: {newest}); {fix}")
        if not offline_mode and repo:
            kind, message = owner_problem(package.name, repo, fetch)
            (problems if kind == "error" else warnings).extend([message] if message else [])
        args = ["package", "--list", "--allow-dirty", "--manifest-path", str(path)]
        result = cargo(args + (["--offline"] if offline_mode else []), base)
        if result.returncode:
            if not problems:   # else most likely one of them
                problems.append(f"{package.name}: `cargo package --list` fails: {cargo_failure(result)}")
        else:
            problems.extend(embed_problems(package.name, crate_dir, result.stdout.split()))
        errors.extend(problems)
    if only is None:
        declared = declared_crates(release, base, found)
        members = {}
        for meta, workspace in found.values():
            if meta["id"] in workspace.get("workspace_members", ()) and meta["name"] not in declared and any(
                    LIBRARY_KINDS & set(t["kind"]) for t in meta["targets"]):
                members[meta["name"]] = " (publish = false)" if meta.get("publish") == [] else ""
        if members:
            warnings.append(f"library crates {CONFIG} does not publish: "
                            + ", ".join(f"{n}{note}" for n, note in sorted(members.items()))
                            + ". To publish one, list it with registry: crates.io; a crate meant to stay "
                            "private needs nothing")
    return errors, warnings


def publish_dry_run(release, base, runner=None):
    """`cargo publish --dry-run --locked` over every crates.io package, as
    the release will run it (one cargo call per workspace, so each crate is
    verified against the others' packages). Builds every crate: slow, and
    meant for CI. Returns an error message, or None."""
    runner = runner or cargo_run
    groups = {}
    for package in release.packages:
        if package.registry == "crates.io":
            manifest = str((base / package.manifest).resolve())
            located = runner(["locate-project", "--workspace", "--message-format", "plain",
                              "--manifest-path", manifest], base)
            workspace = Path(located.stdout.strip() if not located.returncode else manifest).parent
            groups.setdefault(workspace, []).append(package.name)
    for directory, names in groups.items():
        args = ["publish", "--dry-run", "--locked", "--allow-dirty", *[a for n in names for a in ("-p", n)]]
        print(f"$ cargo {' '.join(args)}", flush=True)
        result = runner(args, directory)
        if result.returncode:
            print(result.stderr or "", end="", file=sys.stderr)
            return f"`cargo publish --dry-run` fails for {', '.join(names)}: {cargo_failure(result)}"
    return None


# ---------------------------------------------------------------------------
# Installing: repositories use a copy of the kit, pinned and reviewable: a
# repository's release changes only when it installs a newer kit.

KIT = Path(__file__).resolve().parent.parent
VENDOR = ".github/rab"
BUILD_WORKFLOW = ".github/workflows/rab-build.yml"
MANIFEST = f"{VENDOR}/manifest.json"
# release-kit 1.x's layout: `vendored` still accepts a copy there, with a
# warning, and `install` migrates it (until rab 3.0.0).
OLD_VENDOR = ".github/release-kit"
OLD_BUILD_WORKFLOW = ".github/workflows/release-kit-build.yml"
OLD_MANIFEST = f"{OLD_VENDOR}/manifest.json"
# What `install` rewrites in the repository's own workflows when it migrates.
OLD_PATHS = ((f"{OLD_VENDOR}/", f"{VENDOR}/"), ("release-kit-build.yml", "rab-build.yml"))
NOTICE = """# rab, the Release Asset Bundle (installed copy)

Installed from buchochelliq-labs/release-asset-bundle {version} ({commit}) by
`release.py install`. Do not edit these files: CI checks them against
manifest.json (`python3 .github/rab/scripts/release.py vendored`).
To update, install again from a newer checkout of the kit:

    python3 ../release-asset-bundle/scripts/release.py install

This repository's release is configured in rab.yml at its root.
"""


def kit_files(kit=KIT):
    """{path in the repository: source file} for an install."""
    files = {f"{VENDOR}/scripts/release.py": kit / "scripts" / "release.py",
             BUILD_WORKFLOW: kit / "workflows" / "rab-build.yml"}
    for action in sorted(kit.glob("*/action.yml")):
        files[f"{VENDOR}/{action.parent.name}/action.yml"] = action
    for lock in sorted(kit.glob("*/requirements.txt")):  # what an action installs, hashes pinned
        files[f"{VENDOR}/{lock.parent.name}/requirements.txt"] = lock
    return files


def digest(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def migrate(target):
    """Move a repository from release-kit 1.x's names to rab's: release-kit.yml
    becomes rab.yml (`git mv` when git tracks it), the old installed copy and
    build workflow are removed, and the old paths in the repository's own
    workflows are rewritten. Refuses, changing nothing, when both config names
    exist. Returns what it changed, as lines to print."""
    target = Path(target)
    new, old = target / CONFIG, target / OLD_CONFIG
    if new.is_file() and old.is_file():
        raise RuntimeError(f"both {CONFIG} and {OLD_CONFIG} exist in {target}: keep {CONFIG}, delete "
                           f"{OLD_CONFIG}, and install again")
    changes = []
    if old.is_file():
        tracked = subprocess.run(["git", "ls-files", "--error-unmatch", OLD_CONFIG], cwd=target,
                                 capture_output=True, text=True).returncode == 0
        if tracked:
            subprocess.run(["git", "mv", OLD_CONFIG, CONFIG], cwd=target, check=True, capture_output=True)
        else:
            old.rename(new)
        changes.append(f"renamed {OLD_CONFIG} -> {CONFIG}{' (git mv)' if tracked else ''}")
    if (target / OLD_VENDOR).is_dir():
        shutil.rmtree(target / OLD_VENDOR)
        changes.append(f"removed {OLD_VENDOR}/ (replaced by {VENDOR}/)")
    if (target / OLD_BUILD_WORKFLOW).is_file():
        (target / OLD_BUILD_WORKFLOW).unlink()
        changes.append(f"removed {OLD_BUILD_WORKFLOW} (replaced by {BUILD_WORKFLOW})")
    workflows = target / ".github" / "workflows"
    for workflow in sorted(workflows.glob("*.y*ml")) if workflows.is_dir() else ():
        if workflow.name == PurePosixPath(BUILD_WORKFLOW).name:
            continue
        text = workflow.read_text(encoding="utf-8")
        rewritten, count = text, 0
        for before, after in OLD_PATHS:
            count += rewritten.count(before)
            rewritten = rewritten.replace(before, after)
        if count:
            workflow.write_text(rewritten, encoding="utf-8")
            changes.append(f"rewrote {count} old path{'s' if count > 1 else ''} in "
                           f"{workflow.relative_to(target).as_posix()} ({OLD_VENDOR}/ -> {VENDOR}/, "
                           "release-kit-build.yml -> rab-build.yml)")
    return changes


def install(target=None, kit=KIT):
    """Copy the kit into the repository at `target` (default: the one you are
    in), migrating a release-kit 1.x layout first. Returns the paths written."""
    if not (kit / "workflows" / "rab-build.yml").exists():
        raise RuntimeError("install runs from a checkout of the kit, not from an installed copy: "
                           "git clone https://github.com/buchochelliq-labs/release-asset-bundle")
    target = Path(target) if target else root()
    if target.resolve() == kit.resolve():
        raise RuntimeError("run install from the repository to install into, not from the kit itself")
    version = subprocess.run(["git", "describe", "--tags", "--always", "--dirty"], cwd=kit, text=True,
                             capture_output=True).stdout.strip() or f"v{VERSION}"
    commit = subprocess.run(["git", "rev-parse", "HEAD"], cwd=kit, text=True, capture_output=True).stdout.strip()
    migrated = migrate(target)
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
    manifest = {"source": "buchochelliq-labs/release-asset-bundle", "version": version, "commit": commit,
                "files": {path: digest(target / path) for path in sorted(written)}}
    (target / MANIFEST).write_text(json.dumps(manifest, indent=2) + "\n", encoding="utf-8")
    written += [f"{VENDOR}/README.md", MANIFEST]
    starter = target / ".github" / "workflows" / "release.yml"
    if not starter.exists():
        shutil.copy2(kit / "templates" / "release.yml", starter)
        written.append(".github/workflows/release.yml")
    if migrated:
        print(f"migrated {target} from release-kit 1.x:", flush=True)
        for line in migrated:
            print(f"  {line}", flush=True)
    print(f"installed rab {version} into {target}:", flush=True)
    for path in written:
        print(f"  {path}", flush=True)
    if migrated:
        print("Review the changes (git status, git diff) and commit them together.", flush=True)
    return written


def vendored(target=None):
    """Problems with the installed copy: files edited, missing, or added. A
    copy at release-kit 1.x's .github/release-kit/ is checked too, with a
    warning, when there is none at .github/rab/."""
    target = Path(target) if target else root()
    manifest_path, vendor = MANIFEST, VENDOR
    if not (target / MANIFEST).exists() and (target / OLD_MANIFEST).exists():
        manifest_path, vendor = OLD_MANIFEST, OLD_VENDOR
        warn_once(f"the installed copy is at {OLD_VENDOR}/, release-kit 1.x's path; install rab again to "
                  f"move it to {VENDOR}/ (the old path is accepted until rab 3.0.0)")
    try:
        manifest = json.loads((target / manifest_path).read_text(encoding="utf-8"))
    except FileNotFoundError:
        return [f"{MANIFEST} is missing: install rab (release.py install)"]
    problems = []
    if vendor == VENDOR and (target / OLD_VENDOR).exists():
        problems.append(f"{OLD_VENDOR}/ is left from release-kit 1.x: delete it (install migrates it)")
    for path, expected in manifest["files"].items():
        file = target / path
        if not file.exists():
            problems.append(f"{path} is missing")
        elif digest(file) != expected:
            problems.append(f"{path} was edited; the kit's files change only by installing again")
    known = set(manifest["files"]) | {manifest_path, f"{vendor}/README.md"}
    for file in sorted((target / vendor).rglob("*")):
        relative = file.relative_to(target).as_posix()
        if file.is_file() and relative not in known and "__pycache__" not in relative:
            problems.append(f"{relative} is not part of the kit {manifest['version']}")
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
                continue  # not a release of the kit (from before adoption): not resumed
    return found


def resolve(commit, wanted, remote, existing, binds, published, finishing=None):
    """The commit to release: --commit; else where an unfinished release
    (--release, or the newest tagged) was started; else, for --release, the
    last commit on the default branch naming it; else HEAD. A release is
    unfinished while a package is unpublished, or while `finishing(release,
    commit)` says a channel made from a published one is not done."""
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
        if wanted or any(not published(p) for p in release.packages) or (finishing and finishing(release, candidate)):
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


def wait_for(tag_name, packages, timeout, status=published_on, run=run_of, sleep=time.sleep, stale=None,
             channels=(), channel_status=None, settle=10):
    """Return once the release run succeeded and every package is published.
    `channels` are (package, channel) pairs made from a release; once the rest
    is done they are polled up to `settle` more times, and any not done then is
    named (a run that succeeds without finishing one, e.g. Homebrew without a
    tap token, is not waited on for hours)."""
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
            unfinished = [(p, c, state) for p, c in channels if (state := channel_status(p, c)) not in DONE]
            # A terminal state will not change by waiting.
            if any(s not in TERMINAL for _, _, s in unfinished) and settle > 0 and time.monotonic() <= deadline:
                settle -= 1
                sleep(30)
                continue
            print(f"{tag_name}: published " + ", ".join(str(p) for p in packages), flush=True)
            for package, channel, state in unfinished:
                if state in TERMINAL:
                    print(f"note   {channel} of {package} is {TERMINAL[state]}", flush=True)
                    continue
                print(f"note   {channel} of {package} is {state}, not done: see the run's publish-{channel} job. "
                      "Fix the cause (a missing token or a rejected package), then run `release.py tag` again "
                      f"to finish it.", flush=True)
            return
        if time.monotonic() > deadline:
            still = latest and latest["status"] != "completed"
            raise RuntimeError(f"not published within {timeout // 60} minutes: "
                               + ", ".join(str(p) for p in missing)
                               + (f". The run is still going ({latest['url']}); run `release.py tag` again "
                                  "to keep waiting: it changes nothing while the run is live." if still else ""))
        sleep(30)


def tag(commit=None, remote="origin", timeout=10800, dry_run=False, wanted=None,
        status=published_on, run=run_of, wait=wait_for, channel_status=channel_state):
    """Tag the release on its commit and wait for it to be published. A
    published package whose derived channels (Homebrew, Chocolatey) are not
    done is finished: the release tag is pushed again with only them due."""
    existing = remote_tags(remote)

    def binds(package, tagged):
        # A tag elsewhere holds the release there unless its run provably
        # published nothing; then the tag is moved.
        return status(package) or not dead(run(release_at(tagged, package).tag), tagged)

    def carried_to(sha):
        # The commit the release tag on `sha` was carried from, if it was.
        name = release_at(sha).tag
        if existing.get(name) != sha:
            return None
        ref = f"refs/tags/{name}"
        git("fetch", "--quiet", "--no-tags", remote, f"+{ref}:{ref}")
        return trailers(name).get("Carried-from")

    def carries(tagged, sha):
        # Asked for by --commit only: a release whose run at `tagged` stopped
        # (ended without success, so nothing is uploading) is finished from
        # `sha`, a later commit. The split check has proved that every package
        # tagged there ships the same files here; its published packages keep
        # their records there, the rest are tagged here. Once carried, the
        # release tag on `sha` records it, and a later `tag` keeps finishing
        # it there (with or without --commit) while no run is live.
        if tagged == sha or subprocess.run(["git", "merge-base", "--is-ancestor", tagged, sha],
                                           cwd=root()).returncode:
            return False
        if carried_to(sha) == tagged:
            latest = run(release_at(sha).tag)
            return bool(latest and latest["head_sha"] == sha and latest["status"] == "completed")
        if not commit:
            return False  # nothing to read on `tagged` (it may predate rab.yml)
        latest = run(release_at(tagged).tag)
        return bool(latest and latest["head_sha"] == tagged and latest["status"] == "completed"
                    and latest["conclusion"] != "success")

    here = load(files_on_disk())
    unpublished = [p for p in here.packages if not status(p)]
    if foreign := not_its_own(here, existing, None, remote, unpublished):
        raise RuntimeError(foreign_error(foreign, existing))

    def finishing(release, candidate):
        # A newer release on HEAD replaces an older one's formula and package:
        # going back to finish them would publish a downgrade.
        pending = {p: c for p in release.packages if existing.get(p.tag) == candidate and status(p)
                   and (c := pending_channels(p, lambda q, ch: channel_status(q, ch, candidate)))}
        if pending and version_key(here.name) > version_key(release.name):
            print(f"note   release {release.name} has channels not done ("
                  + "; ".join(f"{p}: {', '.join(c)}" for p, c in pending.items())
                  + f"), but HEAD names release {here.name}: run `tag --release {release.name}` to finish them",
                  flush=True)
            return False
        return bool(pending)

    sha, why = resolve(commit, wanted, remote, existing, binds, status, finishing)
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

    deletions, records, due, channels, carried = [], [], [], {}, None
    for package in release.packages:
        if status(package):
            # Its upload is done; the channels made from it may not be. They
            # are finished from the commit it was released from, or from a
            # later one the release is carried to.
            pending, ended = {}, {}
            tagged = existing.get(package.tag)
            if tagged and (tagged == sha or carries(tagged, sha)):
                if tagged != sha:
                    carried = tagged
                pending = pending_channels(package, lambda p, c: channel_status(p, c, sha), ended)
            for channel, state in ended.items():
                print(f"note   {channel} of {package} is {TERMINAL[state]}; not retried", flush=True)
            if not pending:
                print(f"skip   {package.tag}: {package} is already published", flush=True)
                continue
            print(f"finish {package.tag}: {package} is published; "
                  + ", ".join(f"{c} is {state} ({RETRY[c]})" for c, state in pending.items()), flush=True)
            due.append(package)
            channels[package] = list(pending)
            continue
        tagged = existing.get(package.tag)
        if tagged and tagged != sha:
            if binds(package, tagged) and carries(tagged, sha):
                print(f"carry  {package.tag}: the release run at {tagged[:12]} stopped before publishing it; "
                      f"{sha[:12]} ships the same files", flush=True)
                deletions.append(package.tag)
                tagged, carried = None, tagged
            elif binds(package, tagged):
                # Tagged elsewhere, shipping the same files (the split check
                # passed), and its run there may publish it: wait for that run.
                print(f"wait   {package.tag}: tagged at {tagged[:12]}, which ships the same files; "
                      "waiting for its release run", flush=True)
                if not dry_run:
                    wait(release_at(tagged, package).tag, [package], timeout, status)
                continue
            else:
                print(f"move   {package.tag}: the release run at {tagged[:12]} published nothing", flush=True)
                deletions.append(package.tag)
                tagged = None
        if tagged:
            print(f"keep   {package.tag}: already tagged here", flush=True)
        else:
            print(f"tag    {package.tag} at {sha[:12]} (a record)", flush=True)
            records.append((package.tag, tag_message(str(package), release, sha)))
        due.append(package)
        channels[package] = channels_of(package)
    if not due:
        print(f"Every package of release {release.name} is published.")
        return

    start, stale = None, None
    tagged = existing.get(release.tag)
    latest = run(release.tag) if tagged else None
    if tagged and tagged != sha:
        if dead(latest, tagged):
            print(f"move   {release.tag}: its run at {tagged[:12]} ended {latest['conclusion']} before publishing",
                  flush=True)
        elif carries(tagged, sha):
            print(f"carry  {release.tag}: its run at {tagged[:12]} ended {latest['conclusion']}; the release is "
                  f"finished from {sha[:12]}", flush=True)
            carried = tagged
        else:
            raise RuntimeError(f"{release.tag} is on {tagged[:12]}, not {sha[:12]}, and its run there may have "
                               f"published: release from that commit (--commit {tagged[:12]}) or wait for it")
        deletions.append(release.tag)
        tagged = None
    elif tagged and latest and latest["status"] == "completed" and (
            latest["conclusion"] != "success" or all(status(p) for p in due)):
        # A run that succeeded is run again only to finish derived channels:
        # everything due is published, so the new run uploads nothing else.
        print(f"retag  {release.tag}: its run ended {latest['conclusion']}; the new run publishes only what "
              "is still due", flush=True)
        deletions.append(release.tag)
        stale, tagged = latest["url"], None
    if tagged:
        print(f"wait   {release.tag}: already tagged here", flush=True)
    else:
        subjects = "".join(f"{p}\n" if channels[p] == channels_of(p) else f"{p}: {', '.join(channels[p])}\n"
                           for p in due)
        start = (release.tag, tag_message(f"Release {release.name}\n\n{subjects}".rstrip(), release, sha, carried))
        print(f"tag    {release.tag} at {sha[:12]} (starts the release run: "
              + ", ".join(p.name if channels[p] == channels_of(p) else f"{p.name} ({', '.join(channels[p])})"
                          for p in due) + ")", flush=True)

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
    wait(release.tag, due, timeout, status, stale=stale,
         channels=[(p, c) for p in due for c in channels[p] if c in CHANNEL_KEYS],
         channel_status=lambda p, c: channel_status(p, c, sha))


def status_lines(read=None, status=published_on, remote="origin", channel_status=channel_state):
    release = load(read or files_on_disk())
    try:
        existing = remote_tags(remote)
    except subprocess.CalledProcessError:
        existing = {}
    lines = [f"Release {release.name} (tag {release.tag}: "
             f"{existing[release.tag][:12] if release.tag in existing else 'not tagged'})"]
    for package in release.packages:
        tagged = existing.get(package.tag)
        published = status(package)
        lines.append(f"  {str(package):<48} {package.tag:<28} "
                     f"{'tagged ' + tagged[:12] if tagged else 'untagged':<20} "
                     f"{'published' if published else 'due'}")
        # The channels made from its release: due until it is out, then read live.
        for channel in channels_of(package)[1:]:
            state = channel_status(package, channel, tagged) if published else "due"
            lines.append(f"    {channel:<46} {RETRY[channel]:<49} {state_text(state)}")
    return lines


def main(argv=None):
    parser = argparse.ArgumentParser(prog="release.py", description=__doc__.split("\n\n")[0])
    parser.add_argument("--version", action="version", version=f"rab {VERSION}")
    commands = parser.add_subparsers(dest="command", required=True)
    checker = commands.add_parser("check")
    checker.add_argument("--offline", action="store_true",
                         help=f"skip the checks that ask crates.io (also {OFFLINE}=1)")
    checker.add_argument("--publish-dry-run", action="store_true",
                         help="also run `cargo publish --dry-run` over the crates.io packages (builds them; for CI)")
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
    shaper.add_argument("--options-only", action="store_true", help="only print what rab.yml says")
    renderer = commands.add_parser("render")
    renderer.add_argument("--name", required=True)
    renderer.add_argument("--channel", required=True, choices=sorted(CHANNEL_KEYS))
    renderer.add_argument("--sums", required=True, help="the release's checksums, in sha256sum format")
    renderer.add_argument("--out", required=True)
    refused = commands.add_parser("chocolatey-refused")
    refused.add_argument("--name", required=True)
    refused.add_argument("--log", required=True, help="the output of the failed `choco push`")
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
    uploader = commands.add_parser("pypi-upload")
    uploader.add_argument("--dir", required=True, help="the wheels and sdists to upload")
    uploader.add_argument("--attestations", default="true", choices=["true", "false"],
                          help="sign and upload PEP 740 attestations (default true)")
    recorder = commands.add_parser("record")
    recorder.add_argument("--tag", help="the release tag the run was for")
    recorder.add_argument("--sha", default="", help="the gate's sha output (empty if the gate failed)")
    recorder.add_argument("--selection", default="", help="the gate's selection output")
    recorder.add_argument("--verify-only", default="", choices=["true", "false", ""],
                          help="the gate's verify-only output")
    recorder.add_argument("--needs", default="{}", help="the record job's toJSON(needs)")
    recorder.add_argument("--dist", default="dist", help="where the build artifacts were downloaded")
    recorder.add_argument("--out", default="release-record.json")
    recorder.add_argument("--schema", action="store_true", help="print the record's JSON Schema and stop")
    args = parser.parse_args(argv)
    try:
        if args.command == "check":
            if errors := check(files_on_disk()):
                print("\n".join(errors), file=sys.stderr)
                return 1
            release = load(files_on_disk())
            if any(p.registry == "crates.io" for p in release.packages) or (root() / "Cargo.toml").is_file():
                errors, warnings = crates_io_readiness(release, root(), offline_mode=args.offline or offline())
                for warning in warnings:
                    print(f"warning: {warning}", file=sys.stderr)
                if not errors and args.publish_dry_run:
                    errors = [e for e in [publish_dry_run(release, root())] if e]
                if errors:
                    print("\n".join(errors), file=sys.stderr)
                    return 1
            print(f"{CONFIG} matches the repository (release {release.name}).")
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
        elif args.command == "chocolatey-refused":
            chocolatey_refused(args.name, Path(args.log).read_text(encoding="utf-8", errors="replace"))
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
            where = MANIFEST if (root() / MANIFEST).exists() else OLD_MANIFEST
            manifest = json.loads((root() / where).read_text(encoding="utf-8"))
            print(f"rab {manifest['version']}: the installed copy is unmodified.")
        elif args.command == "preflight":
            # Only whether an Open VSX token is set; the token stays in the environment.
            preflight(args.registry, args.selection, ovsx_token=bool(os.environ.get("OVSX_PAT")),
                      ready=lambda names: crates_io_readiness(load(files_on_disk()), root(), only=names))
        elif args.command == "verify":
            verify(args.registry, args.selection, timeout=args.timeout)
        elif args.command == "consume":
            consume(args.registry, args.selection)
        elif args.command == "publish-crates":
            # Tokens come from the environment, never the command line.
            publish_crates(args.selection, args.new_crates, os.environ.get("RELEASE_TRUSTED_TOKEN", ""),
                           os.environ.get("RELEASE_NEW_CRATE_TOKEN", ""))
        elif args.command == "pypi-upload":
            pypi_upload(args.dir, attestations=args.attestations == "true")
        elif args.command == "record":
            if args.schema:
                print(json.dumps(RECORD_SCHEMA, indent=2))
                return 0
            if not args.tag:
                parser.error("record needs --tag")
            document = record(args.tag, args.sha, args.selection, args.verify_only == "true", args.needs,
                              args.dist)
            Path(args.out).write_text(json.dumps(document, indent=2) + "\n", encoding="utf-8")
            lines = record_lines(document)
            print("\n".join(lines + [f"wrote {args.out}"]))
            if summary := os.environ.get("GITHUB_STEP_SUMMARY"):
                with open(summary, "a", encoding="utf-8") as handle:
                    handle.write("### Release record\n\n```text\n" + "\n".join(lines) + "\n```\n")
    except (RuntimeError, ValueError, subprocess.CalledProcessError) as error:
        print(f"error: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main())
