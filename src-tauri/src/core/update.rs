//! Whether this build is allowed to update itself.
//!
//! Windows and macOS install syncparty as a self-contained application, so the
//! updater swapping it in place is the only mechanism there is. Linux does
//! not: every artifact syncparty ships there — the `.deb` and the AUR package
//! — is owned by a package manager, and there is no build a user installs by
//! hand.
//!
//! Tauri's updater is perfectly capable of `dpkg -i` and `rpm -U`, which is
//! exactly the problem. Installing behind apt's back leaves the package
//! database holding a version apt did not put there, and the conflict surfaces
//! later, on an unrelated upgrade. Arch's packaging guidelines take the same
//! position for the same reason.
//!
//! So on Linux the app still *checks* — being told a new version exists is
//! useful — and then stops, pointing at the package manager instead of
//! guessing which one it is. Naming the wrong command is worse than naming
//! none, and probing `/etc/os-release` to guess correctly is a distro-sniffing
//! habit this codebase deliberately does not have.

use serde::Serialize;
use ts_rs::TS;

#[derive(Debug, Clone, Serialize, TS)]
#[ts(export)]
#[serde(rename_all = "camelCase")]
pub struct UpdatePolicy {
    /// Whether to look for a new release at all.
    pub checks: bool,
    /// Whether the app may download and install what it finds.
    ///
    /// False on Linux, where the answer is "your package manager does that".
    pub self_installs: bool,
}

impl UpdatePolicy {
    pub fn current() -> Self {
        Self {
            checks: true,
            self_installs: !cfg!(target_os = "linux"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_checks_but_never_installs() {
        let policy = UpdatePolicy::current();

        assert!(policy.checks);
        assert_eq!(policy.self_installs, !cfg!(target_os = "linux"));
    }
}
