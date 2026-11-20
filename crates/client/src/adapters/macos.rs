//! macOS adapter: BlackHole as the virtual cable and launchd for autostart.

use std::{env, fs, path::Path, path::PathBuf, process::Command};

use anyhow::{Context, Result, bail};
use headhub_core::audio::{self, Direction};

use crate::ports::{Autostart, Cable, VirtualCable};

const SPEAKER: &str = "BlackHole 2ch";
const MIC: &str = "BlackHole 16ch";
const LABEL: &str = "dev.headhub.agent";

/// BlackHole 2ch is the speaker, BlackHole 16ch the mic; the agent runs as a LaunchAgent.
#[derive(Debug, Default)]
pub struct MacOs;

impl VirtualCable for MacOs {
    fn prepare(&self) -> Result<Cable> {
        let hint =
            "instale com `brew install blackhole-2ch blackhole-16ch` e rode o comando de novo";
        audio::find_device(Some(SPEAKER), Direction::Input).context(hint)?;
        audio::find_device(Some(MIC), Direction::Output).context(hint)?;
        Ok(Cable {
            speaker: SPEAKER.to_owned(),
            mic: MIC.to_owned(),
        })
    }
}

impl Autostart for MacOs {
    fn install(&self, exe: &Path) -> Result<()> {
        let plist = plist_path()?;
        let log = home()?.join("Library/Logs/headhub.log");
        fs::create_dir_all(plist.parent().expect("plist path has a parent"))?;
        fs::write(&plist, plist_body(exe, &log))?;
        // Fails when the agent is not loaded yet, which is the normal first install.
        let _ = launchctl(&["bootout", &domain()?, &plist.to_string_lossy()]);
        launchctl(&["bootstrap", &domain()?, &plist.to_string_lossy()])?;
        println!(
            "serviço instalado: {} (log em {})",
            plist.display(),
            log.display()
        );
        Ok(())
    }

    fn uninstall(&self) -> Result<()> {
        let plist = plist_path()?;
        if plist.exists() {
            // Fails when the agent is already stopped; removing the file is what matters.
            let _ = launchctl(&["bootout", &domain()?, &plist.to_string_lossy()]);
            fs::remove_file(&plist)?;
        }
        Ok(())
    }
}

fn home() -> Result<PathBuf> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME não definido")
}

fn plist_path() -> Result<PathBuf> {
    Ok(home()?.join(format!("Library/LaunchAgents/{LABEL}.plist")))
}

fn domain() -> Result<String> {
    let out = Command::new("id").arg("-u").output()?;
    Ok(format!("gui/{}", String::from_utf8(out.stdout)?.trim()))
}

fn launchctl(args: &[&str]) -> Result<()> {
    let out = Command::new("launchctl").args(args).output()?;
    if !out.status.success() {
        bail!(
            "launchctl {}: {}",
            args[0],
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn plist_body(exe: &Path, log: &Path) -> String {
    let (exe, log) = (exe.display(), log.display());
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{LABEL}</string>
  <key>ProgramArguments</key><array><string>{exe}</string><string>run</string></array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#
    )
}
