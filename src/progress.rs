//! Numbered steps, as `[3/6] Installing runner dependencies`: the desktop app
//! fills a progress bar from them and shows the step's name, while every
//! other line goes to its full output. A step is printed even when it has
//! nothing to do, so the count holds.

/// Prints step `number` of `total`.
pub fn step(number: u8, total: u8, name: &str) {
    println!("[{number}/{total}] {name}");
}

/// The steps of each command.
pub const CONNECT: u8 = 6;
pub const DISCONNECT: u8 = 4;
pub const CLEANUP: u8 = 7;
pub const SYNC: u8 = 2;
