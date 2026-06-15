// SPDX-License-Identifier: GPL-3.0-or-later
//! Generate the choom man pages from the clap command tree into `man/`.
//!
//! Run from the workspace root: `cargo run -p xtask` (or `task man`).

use std::path::Path;

use anyhow::{Context, Result};
use clap_mangen::Man;

fn main() -> Result<()> {
    let out = Path::new("man");
    std::fs::create_dir_all(out).context("create man/")?;

    let cmd = choom_cli::command();

    // Top-level page: man/choom.1
    write_page(out, "choom", &cmd)?;

    // One page per subcommand: man/choom-<sub>.1
    for sub in cmd.get_subcommands() {
        let name = format!("choom-{}", sub.get_name());
        // clap's `Str` wants a 'static name; leak it (this binary is a one-shot tool).
        let static_name: &'static str = Box::leak(name.clone().into_boxed_str());
        let page = sub.clone().name(static_name);
        write_page(out, &name, &page)?;
    }

    println!("wrote man pages to {}/", out.display());
    Ok(())
}

fn write_page(dir: &Path, name: &str, cmd: &clap::Command) -> Result<()> {
    let mut buf = Vec::new();
    Man::new(cmd.clone())
        .render(&mut buf)
        .with_context(|| format!("render {name}"))?;
    let path = dir.join(format!("{name}.1"));
    std::fs::write(&path, buf).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}
