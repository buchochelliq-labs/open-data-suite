//! Opt-in reader smoke test. Prints metadata only, never source text or credentials.
use ods_provider_git::{LocalGitCode, RemoteGitCode};
use ods_sdk::contracts::{
    code_reader::{CodeReader, require_read_only},
    producing_code::{CodeLocation, Revision},
};
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 6 {
        return Err("usage: read_source <local|https> <root|url> <commit> <producing-run> <path> <max-bytes>".into());
    }
    let max_bytes = args[5].parse()?;
    let deadline = Duration::from_secs(10);
    let reader: Box<dyn CodeReader> = match args[0].as_str() {
        "local" => Box::new(LocalGitCode::new(
            BTreeMap::from([("approved".into(), PathBuf::from(&args[1]))]),
            max_bytes,
            deadline,
        )?),
        "https" => Box::new(RemoteGitCode::new(
            BTreeMap::from([("approved".into(), args[1].clone())]),
            max_bytes,
            deadline,
        )?),
        _ => return Err("reader mode must be local or https".into()),
    };
    let code = CodeLocation::new(
        "approved",
        &args[4],
        Revision::Exact {
            commit: args[2].clone(),
            run: args[3].clone(),
        },
    );
    require_read_only(reader.as_ref(), &code).await?;
    let document = reader.read_code(&code, Some(&args[3])).await?;
    println!(
        "commit={} bytes={}",
        document.commit(),
        document.text().len()
    );
    Ok(())
}
