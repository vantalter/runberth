//! The server's processor architecture. Runberth runs on x86_64 and arm64
//! Linux, and the GitHub runner it installs, and the on-demand dock's broker
//! session, must name the same one.

use crate::bail;
use crate::error::Result;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Arch {
    X64,
    Arm64,
}

impl Arch {
    /// From a kernel machine name, as `uname -m` prints it.
    pub fn from_machine(machine: &str) -> Option<Self> {
        match machine {
            "x86_64" | "amd64" => Some(Self::X64),
            "aarch64" | "arm64" => Some(Self::Arm64),
            _ => None,
        }
    }

    /// This server's.
    pub fn host() -> Result<Self> {
        let machine = crate::preflight::machine()?;
        let Some(arch) = Self::from_machine(&machine) else {
            bail!("Runberth runs on x86_64 or arm64 Linux; this server is {machine}");
        };
        Ok(arch)
    }

    /// The name in the runner's release files, as in actions-runner-linux-x64.
    pub fn runner(self) -> &'static str {
        match self {
            Self::X64 => "x64",
            Self::Arm64 => "arm64",
        }
    }

    /// The name GitHub's broker expects from a runner.
    pub fn broker(self) -> &'static str {
        match self {
            Self::X64 => "X64",
            Self::Arm64 => "ARM64",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_names() {
        assert_eq!(Arch::from_machine("x86_64"), Some(Arch::X64));
        assert_eq!(Arch::from_machine("aarch64"), Some(Arch::Arm64));
        assert_eq!(Arch::from_machine("armv7l"), None);
        assert_eq!((Arch::Arm64.runner(), Arch::Arm64.broker()), ("arm64", "ARM64"));
        assert_eq!((Arch::X64.runner(), Arch::X64.broker()), ("x64", "X64"));
    }
}
