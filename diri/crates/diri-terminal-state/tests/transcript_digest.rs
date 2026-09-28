//! Cross-revision equivalence probe for the terminal feed path.
//!
//! Drives one `HeadlessScreen` through a long deterministic stream (styled
//! text, wide and combining characters, links, prompts, scroll regions,
//! erase, insert mode, charsets, alternate screen, synchronized output,
//! resizes and more than 10,000 lines of history) with random read splits.
//! Every observable output is hashed: diffs, snapshots, history, scrollback,
//! `content_seq`, `filled_cells`, cursor, title and progress.
//!
//! Run the same file on two revisions and compare the printed digests:
//!
//! ```sh
//! cargo test --release -p diri-terminal-state --test transcript_digest -- --ignored --nocapture
//! ```
//!
//! It stays below the 4 MiB history byte budget: a history codec change can
//! legitimately change how many rows fit under that budget.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use diri_terminal_state::HeadlessScreen;

#[test]
#[ignore = "cross-revision probe; compare its output between builds"]
fn transcript_digest() {
    let actions: &[&str] = &[
        "\x1b[31;1m",
        "\x1b[38;2;1;2;3;48;5;17m",
        "\x1b[0m",
        "\x1b[7m",
        "\x1b[4:3m\x1b[58;5;9m",
        "\x1b[44m\x1b[K\x1b[0m",
        "\x1b[42m\x1b[2K\r\n",
        "\x1b]8;id=a;https://example.invalid/a\x07",
        "\x1b]8;;\x07",
        "\x1b]133;A\x07$ ",
        "\x1b]0;title\x07",
        "\x1b]9;4;1;40\x07",
        "\x1b[2;6r",
        "\x1b[r",
        "\x1b[3S",
        "\x1b[2T",
        "\x1b[2L",
        "\x1b[1M",
        "\x1b[3@",
        "\x1b[2P",
        "\x1b[4X",
        "\x1b[2J",
        "\x1b[H",
        "\x1b[5;7H",
        "\x1b[99;99H",
        "\x1b[4h",
        "\x1b[4l",
        "\x1b[?7l",
        "\x1b[?7h",
        "\x1b(0lqqk\x1b(B",
        "\x1b[?1049h",
        "\x1b[?1049l",
        "\x1b[?2026h",
        "\x1b[?2026l",
        "\x1b7",
        "\x1b8",
        "\x1bD",
        "\x1bM",
        "\t",
        "\x08",
        "界面 e\u{301} 🦀",
        "abc\x1b[3b",
    ];
    let mut screen = HeadlessScreen::new(40, 10);
    let mut seed = 0x6a09_e667_f3bc_c908u64;
    let mut next = move || {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed
    };
    let mut hasher = DefaultHasher::new();
    let mut pending = Vec::new();
    for step in 0..60_000u32 {
        let random = next();
        match random % 8 {
            0..=3 => pending.extend_from_slice(
                format!(
                    "\x1b[3{}m{step:06}\x1b[0m {}\r\n",
                    random % 8,
                    "log text ".repeat((random >> 8) as usize % 12)
                )
                .as_bytes(),
            ),
            4 => pending.extend_from_slice("x".repeat((random >> 8) as usize % 90).as_bytes()),
            5 | 6 => pending
                .extend_from_slice(actions[(random >> 16) as usize % actions.len()].as_bytes()),
            _ => {
                pending.extend_from_slice("\r\n".repeat(1 + (random >> 8) as usize % 30).as_bytes())
            }
        }
        // Random read boundaries split escapes and UTF-8.
        if random % 4 == 0 {
            let mut rest = &pending[..];
            while !rest.is_empty() {
                let take = (1 + next() as usize % 700).min(rest.len());
                screen.feed(&rest[..take]);
                rest = &rest[take..];
            }
            pending.clear();
            format!("{:?}", screen.grid_update(false)).hash(&mut hasher);
            (screen.content_seq(), screen.filled_cells(), screen.cursor()).hash(&mut hasher);
            (screen.title(), screen.is_alt_screen(), screen.size()).hash(&mut hasher);
            format!("{:?}", screen.progress()).hash(&mut hasher);
        }
        if step % 25_000 == 24_999 {
            // Erase scrollback rarely, so history fills and recycles rows.
            pending.extend_from_slice(b"\x1b[3J");
        }
        if random % 211 == 0 {
            let cols = [2, 13, 40, 80, 133, 300][(random >> 20) as usize % 6];
            let rows = [1, 3, 10, 24, 50][(random >> 28) as usize % 5];
            screen.resize(cols, rows);
            (screen.content_seq(), screen.filled_cells()).hash(&mut hasher);
        }
        if step % 1500 == 0 {
            format!("{:?}", screen.full_snapshot()).hash(&mut hasher);
            format!("{:?}", screen.history_snapshot()).hash(&mut hasher);
            format!("{:?}", screen.scrollback()).hash(&mut hasher);
            println!(
                "step {step:6}: {:016x} history {}",
                hasher.finish(),
                screen.history_snapshot().len()
            );
        }
    }
    println!("final: {:016x}", hasher.finish());
}
