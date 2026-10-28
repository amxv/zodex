//! Versioned, immutable macOS Keychain broker for Zodex Local.
//!
//! This binary is installed once as `zodex-keychain-v1` and is deliberately
//! never overwritten by operator updates. The login Keychain therefore keeps
//! authorizing the same code hash even as the main `zodex` binary changes.
//! Only a valid, running Zodex executable in the broker's own directory may
//! request a credential; the key never appears in argv or on disk.

use std::env;
use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result, bail};
use nix::unistd::getppid;
use security_framework::os::macos::code_signing::{
    Flags, GuestAttributes, SecCode, SecRequirement,
};
use security_framework::passwords::{PasswordOptions, generic_password};

const SERVICE: &str = "com.amxv.zodex.local.v2";
const LEGACY_SERVICE: &str = "com.amxv.zodex.local";
const ACCOUNT: &str = "openai-tunnel-runtime-key";
const MAX_KEY_BYTES: u64 = 16 * 1024;
const NOT_FOUND: i32 = -25_300;
const REQUIREMENT: &str = "identifier \"com.amxv.zodex.local\"";

fn authenticate_parent() -> Result<()> {
    let parent = getppid();
    if parent.as_raw() <= 1 {
        bail!("Keychain broker requires a living Zodex parent process");
    }

    // Check the LIVE process's code signature, not a caller-supplied path or
    // command-line argument. In addition to signature validation, constrain
    // it to our actual installed `zodex` sibling. The release operator is
    // ad-hoc signed with this identifier; machine-local signed builds share
    // the identifier but retain their per-machine certificate across updates.
    let mut attrs = GuestAttributes::new();
    attrs.set_pid(parent.as_raw());
    let code = SecCode::copy_guest_with_attribues(None, &attrs, Flags::NONE)
        .context("could not identify the Zodex parent process")?;
    let requirement: SecRequirement = REQUIREMENT
        .parse()
        .context("invalid Zodex signing requirement")?;
    code.check_validity(Flags::NONE, &requirement)
        .context("parent is not a valid signed Zodex executable")?;

    let running_path = code
        .path(Flags::NONE)?
        .to_path()
        .context("parent has no executable path")?;
    let installed_broker = env::current_exe()?.canonicalize()?;
    let folder = installed_broker
        .parent()
        .context("broker has no install directory")?;
    let expected = folder.join("zodex");
    if running_path != expected
        && running_path.canonicalize().ok().as_deref() != Some(expected.as_path())
    {
        bail!("Keychain broker can only be invoked by its installed Zodex sibling");
    }
    if !Path::new(&expected).is_file() {
        bail!("installed Zodex executable is missing");
    }
    Ok(())
}

fn read_item(service: &str) -> Result<Option<Vec<u8>>> {
    match generic_password(PasswordOptions::new_generic_password(service, ACCOUNT)) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.code() == NOT_FOUND => Ok(None),
        Err(error) => Err(error).context("failed to read tunnel credential from Keychain"),
    }
}

fn validate_key(bytes: &[u8]) -> Result<()> {
    if bytes.is_empty()
        || bytes.len() > MAX_KEY_BYTES as usize
        || bytes.contains(&0)
        || bytes.contains(&b'\n')
        || bytes.contains(&b'\r')
        || std::str::from_utf8(bytes).is_err()
    {
        bail!("invalid tunnel credential length or format");
    }
    Ok(())
}

fn set_item(bytes: &[u8]) -> Result<()> {
    validate_key(bytes)?;
    security_framework::passwords::set_generic_password(SERVICE, ACCOUNT, bytes)
        .context("failed to store tunnel credential in Keychain")
}

fn delete_item(service: &str) -> Result<()> {
    match security_framework::passwords::delete_generic_password(service, ACCOUNT) {
        Ok(()) => Ok(()),
        Err(error) if error.code() == NOT_FOUND => Ok(()),
        Err(error) => Err(error).context("failed to remove tunnel credential from Keychain"),
    }
}

fn execute(command: &str) -> Result<()> {
    // No unauthenticated operation (including a key-presence check) can read
    // Keychain. `protocol` prints only a constant, for installer health checks.
    if command == "protocol" {
        println!("1");
        return Ok(());
    }
    authenticate_parent()?;
    match command {
        "get" => {
            let value = match read_item(SERVICE)? {
                Some(value) => Some(value),
                None => {
                    let legacy = read_item(LEGACY_SERVICE)?;
                    if let Some(ref value) = legacy {
                        set_item(value)?;
                    }
                    legacy
                }
            };
            let mut output = std::io::stdout().lock();
            if let Some(value) = value {
                validate_key(&value)?;
                output.write_all(b"1\n")?;
                output.write_all(&value)?;
            } else {
                output.write_all(b"0\n")?;
            }
            output.flush()?;
        }
        "set" => {
            let mut value = Vec::new();
            std::io::stdin()
                .take(MAX_KEY_BYTES + 1)
                .read_to_end(&mut value)?;
            set_item(&value)?;
            println!("ok");
        }
        "delete" => {
            // Both copies are removed, even if deleting one fails.
            let primary = delete_item(SERVICE);
            let legacy = delete_item(LEGACY_SERVICE);
            primary.and(legacy)?;
            println!("ok");
        }
        "check-caller" => println!("ok"), // never reads a credential
        _ => bail!("unknown Keychain broker operation"),
    }
    Ok(())
}

fn main() {
    let args: Vec<_> = env::args().collect();
    let result = if args.len() == 2 {
        execute(&args[1])
    } else {
        Err(anyhow::anyhow!("expected one Keychain broker operation"))
    };
    if let Err(error) = result {
        // Errors must never include the credential or echo stdin.
        eprintln!("Zodex Keychain broker: {error:#}");
        std::process::exit(1);
    }
}
