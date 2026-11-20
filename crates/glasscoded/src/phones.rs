//! Phones that may reach this server: their keys in `~/.ssh/authorized_keys`, each limited to
//! `glasscoded gate` — Glasscode's own calls and session terminals, no shell, no forwarding.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use anyhow::{Context as _, bail};
use glasscode_core::paths;
use glasscode_core::remote::{PhoneKey, PhonePairing};

/// Marks our lines (and carries the phone's name) in the key's comment.
const MARK: &str = "glasscode-phone:";
const KINDS: [&str; 3] = ["ecdsa-sha2-nistp256", "ssh-ed25519", "ecdsa-sha2-nistp384"];

fn authorized_keys() -> PathBuf {
    paths::home().join(".ssh").join("authorized_keys")
}

/// (`ecdsa-sha2-nistp256`, base64 blob) from an OpenSSH public key line.
fn parse_key(key: &str) -> anyhow::Result<(String, String)> {
    let mut parts = key.split_whitespace();
    let (Some(kind), Some(blob)) = (parts.next(), parts.next()) else { bail!("not an SSH public key") };
    if !KINDS.contains(&kind) {
        bail!("{kind} keys are not accepted");
    }
    if blob.len() < 60 || !blob.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')) {
        bail!("the key is malformed");
    }
    Ok((kind.to_string(), blob.to_string()))
}

fn clean_name(name: &str) -> String {
    let name: String = name.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '-' }).collect();
    let name = name.split('-').filter(|p| !p.is_empty()).collect::<Vec<_>>().join("-");
    if name.is_empty() { "iPhone".into() } else { name.chars().take(40).collect() }
}

fn read_lines() -> Vec<String> {
    std::fs::read_to_string(authorized_keys()).map(|t| t.lines().map(str::to_string).collect()).unwrap_or_default()
}

fn write_lines(lines: &[String]) -> anyhow::Result<()> {
    let path = authorized_keys();
    let dir = path.parent().context("no ~/.ssh")?;
    std::fs::create_dir_all(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    let mut text = lines.join("\n");
    text.push('\n');
    let tmp = path.with_extension("glasscode-tmp");
    std::fs::write(&tmp, text)?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// The server's SSH host keys, for the phone to pin on its first connection.
fn host_keys() -> Vec<String> {
    ["ed25519", "ecdsa", "rsa"]
        .iter()
        .filter_map(|kind| std::fs::read_to_string(format!("/etc/ssh/ssh_host_{kind}_key.pub")).ok())
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            Some(format!("{} {}", parts.next()?, parts.next()?))
        })
        .collect()
}

/// A phone's `authorized_keys` line: its key may only run `glasscoded gate`, as its device
/// (M13.2), the id the gate names its connection with.
fn key_line(exe: &str, kind: &str, blob: &str, name: &str, device: &str) -> anyhow::Result<String> {
    if exe.contains('"') || exe.contains('\n') {
        bail!("glasscoded's path cannot be used in authorized_keys");
    }
    if !glasscode_core::caller::device_id_ok(device) {
        bail!("{device:?} is not a device id");
    }
    Ok(format!("command=\"{exe} gate --device {device}\",no-port-forwarding,no-agent-forwarding,no-X11-forwarding,no-user-rc {kind} {blob} {MARK}{}", clean_name(name)))
}

/// The lines after pairing a key: every other line exactly as it was (a line written before
/// M13.2 keeps running `gate` as the device `unknown`; one mistake there locks a phone out), and
/// the new key's line in place of an earlier one for the same key.
fn with_key(lines: Vec<String>, blob: &str, line: String) -> Vec<String> {
    let mut lines: Vec<String> = lines.into_iter().filter(|l| !l.contains(blob)).collect();
    lines.push(line);
    lines
}

/// Allow a phone's key (replacing an earlier line for the same key).
pub fn add(phone: &PhoneKey) -> anyhow::Result<PhonePairing> {
    let (kind, blob) = parse_key(&phone.key)?;
    let exe = paths::root().join("bin").join("glasscoded");
    let exe = if exe.is_file() { exe } else { std::env::current_exe()? };
    let device = glasscode_core::caller::device_id_for_key(&blob);
    let line = key_line(&exe.display().to_string(), &kind, &blob, &phone.name, &device)?;
    write_lines(&with_key(read_lines(), &blob, line))?;
    crate::daemon::log(format!("phone {} may now reach this server", clean_name(&phone.name)));
    Ok(PhonePairing { user: std::env::var("USER").unwrap_or_default(), host: crate::daemon::hostname(), host_keys: host_keys() })
}

pub fn list() -> Vec<PhoneKey> {
    read_lines()
        .iter()
        .filter_map(|line| {
            let name = line.split(MARK).nth(1)?.trim().to_string();
            let words: Vec<&str> = line.split_whitespace().collect();
            let at = words.iter().position(|w| KINDS.contains(w))?;
            Some(PhoneKey { name, key: format!("{} {}", words[at], words.get(at + 1)?) })
        })
        .collect()
}

pub fn remove(key: &str) -> anyhow::Result<()> {
    let (_, blob) = parse_key(key)?;
    let lines: Vec<String> = read_lines().into_iter().filter(|l| !(l.contains(&blob) && l.contains(MARK))).collect();
    write_lines(&lines)
}

#[cfg(test)]
mod tests {
    #[test]
    fn checks_keys_and_names() {
        let blob = "AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBHx9c2FtcGxla2V5Zm9ydGVzdHM";
        assert!(super::parse_key(&format!("ecdsa-sha2-nistp256 {blob}")).is_ok());
        assert!(super::parse_key(&format!("ssh-rsa {blob}")).is_err());
        assert!(super::parse_key("ecdsa-sha2-nistp256 short").is_err());
        // A quote glued to the key could break out of authorized_keys' options: refused.
        assert!(super::parse_key(&format!("ecdsa-sha2-nistp256 {blob}\" evil")).is_err());
        assert_eq!(super::clean_name("Alex's iPhone 17"), "Alex-s-iPhone-17");
        assert_eq!(super::clean_name("\"; rm -rf ~"), "rm-rf");
    }

    #[test]
    fn a_new_pairing_names_its_device() {
        let blob = "AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBHx9c2FtcGxla2V5Zm9ydGVzdHM";
        let device = glasscode_core::caller::device_id_for_key(blob);
        let line = super::key_line("/srv/u/.glasscode/bin/glasscoded", "ecdsa-sha2-nistp256", blob, "Alex's iPhone", &device).unwrap();
        assert_eq!(line, format!("command=\"/srv/u/.glasscode/bin/glasscoded gate --device {device}\",no-port-forwarding,no-agent-forwarding,no-X11-forwarding,no-user-rc ecdsa-sha2-nistp256 {blob} glasscode-phone:Alex-s-iPhone"));
        // What sshd runs splits into the gate's own arguments.
        assert_eq!(line.split('"').nth(1).unwrap().split_whitespace().skip(1).collect::<Vec<_>>(), ["gate", "--device", device.as_str()]);
        assert!(super::key_line("/a\"b/glasscoded", "ssh-ed25519", blob, "x", &device).is_err());
        assert!(super::key_line("/bin/glasscoded", "ssh-ed25519", blob, "x", "a b").is_err());
    }

    #[test]
    fn old_authorized_keys_lines_are_never_rewritten() {
        let old_blob = "AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBolderphonekeyfortests0000";
        let new_blob = "AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBnewerphonekeyfortests0000";
        let old = format!("command=\"/srv/u/.glasscode/bin/glasscoded gate\",no-port-forwarding,no-agent-forwarding,no-X11-forwarding,no-user-rc ecdsa-sha2-nistp256 {old_blob} glasscode-phone:Old-iPhone");
        let mine = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIpersonalkey you@laptop".to_string();
        let line = super::key_line("/srv/u/.glasscode/bin/glasscoded", "ecdsa-sha2-nistp256", new_blob, "New", &glasscode_core::caller::device_id_for_key(new_blob)).unwrap();
        let lines = super::with_key(vec![mine.clone(), old.clone()], new_blob, line.clone());
        assert_eq!(lines, [mine.clone(), old.clone(), line.clone()]);
        // The same key paired again: its line is replaced, with the same device.
        let again = super::with_key(lines, new_blob, line.clone());
        assert_eq!(again, [mine, old, line]);
    }
}
