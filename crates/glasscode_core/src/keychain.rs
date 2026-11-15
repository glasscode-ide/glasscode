//! Secrets in the store of the operating system (`sys::secrets`): the macOS Keychain, or
//! Windows Credential Manager.

/// The store, as messages name it: "the Keychain" or "Windows Credential Manager".
pub use crate::sys::secrets::NAME;

pub fn read(service: &str, account: &str) -> Option<String> {
    crate::sys::secrets::read(service, account)
}

pub fn write(service: &str, account: &str, secret: &str) -> anyhow::Result<()> {
    crate::sys::secrets::write(service, account, secret)
}

pub fn delete(service: &str, account: &str) {
    crate::sys::secrets::delete(service, account)
}
