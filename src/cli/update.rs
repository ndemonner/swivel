//! Self-update from GitHub releases.
//!
//! `swivel update` asks GitHub for the newest release, downloads the binary,
//! checks its signature, and replaces its own executable. The running process
//! keeps its inode, so nothing crashes; the new version starts on the next
//! launch.
//!
//! The signature check is the point, not an ornament. This program opens
//! microphones. A compromised update channel is remote microphone access, so
//! an update must prove it was signed with the project's release key, which is
//! compiled into the binary. TLS and the GitHub account are not enough.
//!
//! The version check needs no API and no JSON. GitHub answers
//! `releases/latest` with a redirect to `releases/tag/<tag>`, and the tag
//! carries the version. Downloads use `curl`, which every macOS ships, and
//! which does not set the quarantine flag that makes Gatekeeper refuse a
//! browser download.

use std::path::{Path, PathBuf};
use std::process::Command;

use data_encoding::HEXLOWER;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};

use crate::config::{RELEASE_PUBKEY_HEX, UPDATE_REPO};
use crate::error::{Error, Result};

/// The version this binary was built as.
pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

/// `swivel update`, and `swivel update --check`.
pub fn update(check_only: bool) -> Result<()> {
    let base = release_base();
    let current = parse_version(CURRENT)
        .ok_or_else(|| Error::Update(format!("this build's own version {CURRENT} is malformed")))?;

    let tag = latest_tag(&base)?;
    let latest = parse_version(&tag)
        .ok_or_else(|| Error::Update(format!("the latest release tag {tag} is not a version")))?;

    if latest <= current {
        println!("swivel {CURRENT} is up to date. The latest release is {tag}.");
        return Ok(());
    }

    if check_only {
        println!("swivel {CURRENT} is installed and {tag} is available.");
        println!("Run `swivel update` to install it.");
        return Ok(());
    }

    let exe = running_binary()?;
    let dir = exe
        .parent()
        .ok_or_else(|| Error::Update("the running binary has no parent directory".into()))?;

    // Fail on a read-only install location before spending the download.
    let probe = dir.join(".swivel-update-probe");
    if std::fs::write(&probe, b"").is_err() {
        return Err(Error::Update(format!(
            "cannot write to {}. Move the binary somewhere you own, or rerun with sudo.",
            dir.display()
        )));
    }
    let _ = std::fs::remove_file(&probe);

    println!("downloading swivel {tag}");
    let staged = dir.join(format!(".swivel-update-{}", std::process::id()));
    let result = download_and_install(&base, &tag, &staged, &exe);
    let _ = std::fs::remove_file(&staged);
    let old = result?;

    println!("installed swivel {tag} at {}", exe.display());
    println!("the previous version is kept at {}", old.display());
    println!("restart swivel to use it. If the menu bar icon is running, quit it first.");
    Ok(())
}

/// Downloads, verifies, and swaps. Returns where the old binary went.
///
/// This is separate so the caller can delete the staged file on every exit
/// path with one line.
fn download_and_install(base: &str, tag: &str, staged: &Path, exe: &Path) -> Result<PathBuf> {
    fetch(&format!("{base}/releases/download/{tag}/swivel"), staged)?;

    let sig_path = staged.with_extension("sig");
    let sig_result = fetch(
        &format!("{base}/releases/download/{tag}/swivel.sig"),
        &sig_path,
    );
    let signature = sig_result.and_then(|()| std::fs::read_to_string(&sig_path).map_err(Error::Io));
    let _ = std::fs::remove_file(&sig_path);

    verify(staged, signature?.trim())?;

    // The signature proves the bytes. Now make them runnable and swap them in.
    // Rename is atomic on one volume, and the staged file is already in the
    // destination directory to guarantee that.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(staged, std::fs::Permissions::from_mode(0o755))?;
    }

    let old = exe.with_extension("old");
    std::fs::rename(exe, &old)?;
    if let Err(e) = std::fs::rename(staged, exe) {
        // Put the working binary back rather than leave nothing installed.
        let _ = std::fs::rename(&old, exe);
        return Err(Error::Update(format!("cannot install the new binary: {e}")));
    }
    Ok(old)
}

/// Checks a file against the project release key.
fn verify(file: &Path, signature_hex: &str) -> Result<()> {
    let key = release_pubkey()?;

    let sig_bytes: [u8; 64] = HEXLOWER
        .decode(signature_hex.as_bytes())
        .map_err(|_| Error::Update("the signature file is not hex".into()))?
        .try_into()
        .map_err(|_| Error::Update("the signature has the wrong length".into()))?;
    let signature = Signature::from_bytes(&sig_bytes);

    let bytes = std::fs::read(file)?;
    key.verify_strict(&bytes, &signature).map_err(|_| {
        Error::Update(
            "the download does not match the project's release signature. \
             Refusing to install it."
                .into(),
        )
    })
}

/// The compiled-in release public key.
fn release_pubkey() -> Result<VerifyingKey> {
    let bytes: [u8; 32] = HEXLOWER
        .decode(RELEASE_PUBKEY_HEX.as_bytes())
        .map_err(|_| Error::Update("the compiled-in release key is not hex".into()))?
        .try_into()
        .map_err(|_| Error::Update("the compiled-in release key has the wrong length".into()))?;
    VerifyingKey::from_bytes(&bytes)
        .map_err(|_| Error::Update("the compiled-in release key is not a valid key".into()))
}

/// Where releases live. The environment override exists for tests only.
fn release_base() -> String {
    std::env::var("SWIVEL_UPDATE_BASE")
        .unwrap_or_else(|_| format!("https://github.com/{UPDATE_REPO}"))
}

/// The newest release tag, read from the `releases/latest` redirect.
fn latest_tag(base: &str) -> Result<String> {
    let url = format!("{base}/releases/latest");
    let out = Command::new("curl")
        .args(["-fsS", "-o", "/dev/null", "-w", "%{redirect_url}", &url])
        .output()
        .map_err(|e| Error::Update(format!("cannot run curl: {e}")))?;

    if !out.status.success() {
        return Err(Error::Update(format!(
            "cannot reach {url}: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )));
    }

    let redirect = String::from_utf8_lossy(&out.stdout).trim().to_string();
    match redirect.rsplit_once("/tag/") {
        Some((_, tag)) if !tag.is_empty() => Ok(tag.to_string()),
        _ => Err(Error::Update(format!(
            "{url} did not point at a release. Has one been published?"
        ))),
    }
}

/// Downloads one file.
fn fetch(url: &str, to: &Path) -> Result<()> {
    let status = Command::new("curl")
        .args(["-fSL", "--retry", "2", "--progress-bar", "-o"])
        .arg(to)
        .arg(url)
        .status()
        .map_err(|e| Error::Update(format!("cannot run curl: {e}")))?;

    if !status.success() {
        return Err(Error::Update(format!("the download of {url} failed")));
    }
    Ok(())
}

/// The file to replace: the running executable, with symlinks resolved.
fn running_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe()?;
    Ok(exe.canonicalize()?)
}

/// `v0.2.1` or `0.2.1` to a comparable triple.
fn parse_version(text: &str) -> Option<(u64, u64, u64)> {
    let text = text.strip_prefix('v').unwrap_or(text);
    let mut parts = text.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((major, minor, patch))
}

// ---------------------------------------------------------------------------
// Maintainer commands. Hidden from --help.
// ---------------------------------------------------------------------------

/// Where the release signing key lives on the maintainer's machine.
fn signing_key_path() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("SWIVEL_RELEASE_KEY") {
        return Ok(PathBuf::from(p));
    }
    let db = crate::store::default_path()?;
    let dir = db
        .parent()
        .ok_or_else(|| Error::Update("the store path has no parent directory".into()))?;
    Ok(dir.join("release-signing.key"))
}

/// `swivel release-keygen`. Creates the signing key, once.
pub fn release_keygen() -> Result<()> {
    let path = signing_key_path()?;
    if path.exists() {
        return Err(Error::Update(format!(
            "{} already exists. Refusing to overwrite a signing key.",
            path.display()
        )));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }

    let secret: [u8; 32] = rand::random();
    let key = SigningKey::from_bytes(&secret);

    std::fs::write(&path, HEXLOWER.encode(&secret))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    println!("wrote {}", path.display());
    println!(
        "public key: {}",
        HEXLOWER.encode(&key.verifying_key().to_bytes())
    );
    println!("Put the public key in RELEASE_PUBKEY_HEX in src/config.rs.");
    println!("Back the key file up somewhere private. Without it you cannot release.");
    Ok(())
}

/// `swivel release-sign <file>`. Writes `<file>.sig`.
pub fn release_sign(file: &Path) -> Result<()> {
    let path = signing_key_path()?;
    let hex = std::fs::read_to_string(&path)
        .map_err(|e| Error::Update(format!("cannot read {}: {e}", path.display())))?;
    let secret: [u8; 32] = HEXLOWER
        .decode(hex.trim().as_bytes())
        .map_err(|_| Error::Update("the signing key file is not hex".into()))?
        .try_into()
        .map_err(|_| Error::Update("the signing key has the wrong length".into()))?;
    let key = SigningKey::from_bytes(&secret);

    // Signing with a key the binary does not trust would publish a release
    // that every client refuses. Catch that here, not after the upload.
    let embedded = release_pubkey()?;
    if key.verifying_key() != embedded {
        return Err(Error::Update(
            "this key does not match RELEASE_PUBKEY_HEX in config.rs. \
             A release signed with it would be refused by every client."
                .into(),
        ));
    }

    let bytes = std::fs::read(file)?;
    let signature = key.sign(&bytes);

    let out = file_with_sig_extension(file);
    std::fs::write(&out, HEXLOWER.encode(&signature.to_bytes()))?;
    println!("wrote {}", out.display());
    Ok(())
}

/// `swivel` has no extension, so `.sig` is appended rather than substituted.
fn file_with_sig_extension(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_os_string();
    name.push(".sig");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_parse_with_and_without_the_prefix() {
        assert_eq!(parse_version("v1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("1.2.3"), Some((1, 2, 3)));
        assert_eq!(parse_version("v10.0.100"), Some((10, 0, 100)));
    }

    #[test]
    fn malformed_versions_are_refused() {
        assert_eq!(parse_version("1.2"), None);
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("release-1"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn version_triples_order_correctly() {
        // Tuple comparison must match semver ordering, or an update loops or
        // never fires.
        assert!(parse_version("0.10.0") > parse_version("0.9.9"));
        assert!(parse_version("1.0.0") > parse_version("0.99.99"));
        assert!(parse_version("0.2.1") > parse_version("0.2.0"));
    }

    #[test]
    fn the_build_version_parses() {
        assert!(
            parse_version(CURRENT).is_some(),
            "CARGO_PKG_VERSION must stay three plain numbers or update breaks"
        );
    }

    #[test]
    fn a_signature_round_trips() {
        let secret: [u8; 32] = rand::random();
        let key = SigningKey::from_bytes(&secret);
        let message = b"one release artifact";

        let signature = key.sign(message);
        assert!(
            key.verifying_key()
                .verify_strict(message, &signature)
                .is_ok()
        );

        // One flipped bit must fail.
        let mut tampered = message.to_vec();
        tampered[0] ^= 1;
        assert!(
            key.verifying_key()
                .verify_strict(&tampered, &signature)
                .is_err()
        );
    }

    #[test]
    fn the_embedded_public_key_is_valid() {
        release_pubkey().expect("RELEASE_PUBKEY_HEX must decode to a real key");
    }

    #[test]
    fn the_sig_name_appends_rather_than_replaces() {
        assert_eq!(
            file_with_sig_extension(Path::new("/a/swivel")),
            Path::new("/a/swivel.sig")
        );
        // A dotted name must not lose part of itself.
        assert_eq!(
            file_with_sig_extension(Path::new("/a/swivel-0.2.0")),
            Path::new("/a/swivel-0.2.0.sig")
        );
    }
}
