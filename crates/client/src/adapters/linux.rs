//! Linux adapter: PulseAudio/PipeWire null sinks as the virtual cable and a systemd user unit.

use std::{env, fs, path::Path, path::PathBuf, process::Command};

use anyhow::{Context, Result, bail};

use crate::ports::{Autostart, Cable, VirtualCable};

/// Apps play into this sink; the agent captures its monitor.
const SPEAKER_SINK: &str = "headhub_speaker";
/// The agent plays the headset mic into this sink...
const MIC_SINK: &str = "headhub_mic_sink";
/// ...and apps record from this source, a remap of the sink monitor (apps often hide monitors).
const MIC_SOURCE: &str = "headhub_mic";
const UNIT: &str = "headhub.service";

/// Null sinks created with `pactl` (works on PulseAudio and pipewire-pulse); systemd for autostart.
#[derive(Debug, Default)]
pub struct Linux;

impl VirtualCable for Linux {
    fn prepare(&self) -> Result<Cable> {
        // cpal finds the PulseAudio socket only through these; without them it silently uses ALSA.
        if env::var_os("XDG_RUNTIME_DIR").is_none() && env::var_os("PULSE_SERVER").is_none() {
            bail!("XDG_RUNTIME_DIR não definido; rode o headhub dentro da sessão do usuário");
        }
        // Modules loaded by pactl vanish when the sound server restarts, so this runs on every start.
        ensure_module(
            "sinks",
            SPEAKER_SINK,
            &[
                "module-null-sink",
                &format!("sink_name={SPEAKER_SINK}"),
                "sink_properties=device.description=headhub-speaker",
            ],
        )?;
        ensure_module(
            "sinks",
            MIC_SINK,
            &[
                "module-null-sink",
                &format!("sink_name={MIC_SINK}"),
                "sink_properties=device.description=headhub-mic-sink",
            ],
        )?;
        ensure_module(
            "sources",
            MIC_SOURCE,
            &[
                "module-remap-source",
                &format!("master={MIC_SINK}.monitor"),
                &format!("source_name={MIC_SOURCE}"),
                "source_properties=device.description=headhub-mic",
            ],
        )?;
        // Matched against cpal names (device descriptions); the monitor's "Monitor of" prefix is
        // localized, so only the sink part is used.
        Ok(Cable {
            speaker: "headhub-speaker".to_owned(),
            mic: "headhub-mic-sink".to_owned(),
        })
    }
}

impl Autostart for Linux {
    fn install(&self, exe: &Path) -> Result<()> {
        let unit = unit_path()?;
        fs::create_dir_all(unit.parent().expect("unit path has a parent"))?;
        fs::write(&unit, unit_body(exe))?;
        systemctl(&["daemon-reload"])?;
        systemctl(&["enable", UNIT])?;
        systemctl(&["restart", UNIT])?;
        println!(
            "serviço instalado: {} (log: journalctl --user -u headhub -f)",
            unit.display()
        );
        Ok(())
    }

    fn uninstall(&self) -> Result<()> {
        let unit = unit_path()?;
        if unit.exists() {
            let _ = systemctl(&["disable", "--now", UNIT]);
            fs::remove_file(&unit)?;
            systemctl(&["daemon-reload"])?;
        }
        Ok(())
    }
}

fn ensure_module(kind: &str, name: &str, args: &[&str]) -> Result<()> {
    let listed = pactl(&["list", "short", kind])?;
    if listed
        .lines()
        .any(|line| line.split('\t').nth(1) == Some(name))
    {
        return Ok(());
    }
    let mut load = vec!["load-module"];
    load.extend_from_slice(args);
    pactl(&load)?;
    Ok(())
}

fn pactl(args: &[&str]) -> Result<String> {
    let out = Command::new("pactl").args(args).output().context(
        "pactl não encontrado; instale pulseaudio-utils (PipeWire precisa do pipewire-pulse)",
    )?;
    if !out.status.success() {
        bail!(
            "pactl {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8(out.stdout)?)
}

fn unit_path() -> Result<PathBuf> {
    let home = env::var_os("HOME").context("HOME não definido")?;
    Ok(PathBuf::from(home).join(".config/systemd/user").join(UNIT))
}

fn systemctl(args: &[&str]) -> Result<()> {
    let out = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()?;
    if !out.status.success() {
        bail!(
            "systemctl --user {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

fn unit_body(exe: &Path) -> String {
    format!(
        "[Unit]
Description=head-hub: headset compartilhado
After=pipewire-pulse.service pulseaudio.service

[Service]
ExecStart={} run
Restart=always
RestartSec=2

[Install]
WantedBy=default.target
",
        exe.display()
    )
}
