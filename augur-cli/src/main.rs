use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use augur_core::{
    camera::EventCamera,
    config::{CameraConfig, RoiConfig},
    metadata::{RecordingAnnotations, RecordingMetadata},
    pipeline::{spawn_pipeline, Evt3CorePreviewDecoder, PipelineOptions},
};
use augur_prophesee::evk4::Evk4Camera;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "augur",
    version,
    about = "AugurRS — Prophesee EVK4/IMX636 event camera CLI"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Status {
        #[arg(long)]
        config: Option<PathBuf>,
    },
    Record {
        output: PathBuf,
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long)]
        duration_s: Option<u64>,
        #[arg(long)]
        experiment_id: Option<String>,
        #[arg(long)]
        operator: Option<String>,
        #[arg(long)]
        notes: Option<String>,
    },
    Config {
        #[command(subcommand)]
        cmd: ConfigCommand,
    },
}

#[derive(Subcommand, Debug)]
enum ConfigCommand {
    Show {
        #[arg(long)]
        config: Option<PathBuf>,
    },
    SetBias {
        key: String,
        value: i32,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    SetRoi {
        x: u16,
        y: u16,
        width: u16,
        height: u16,
        #[arg(long)]
        config: Option<PathBuf>,
    },
    SetMask {
        x: u16,
        y: u16,
        #[arg(long)]
        config: Option<PathBuf>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.cmd {
        Command::Status { config } => status(config),
        Command::Record {
            output,
            config,
            duration_s,
            experiment_id,
            operator,
            notes,
        } => record(
            output,
            config,
            duration_s,
            RecordingAnnotations {
                experiment_id,
                operator,
                notes,
            }
            .normalized(),
        ),
        Command::Config { cmd } => config_cmd(cmd),
    }
}

fn status(config_path: Option<PathBuf>) -> Result<()> {
    let cfg = load_config(config_path.as_deref())?;
    let camera = Evk4Camera::open_imx636().context("failed opening EVK4")?;
    let info = camera.device_info();

    println!(
        "[{} / {}]",
        info.model,
        info.compatible.as_deref().unwrap_or_default()
    );
    println!("vendor: {}", info.vendor);
    if let Some(serial) = info.serial {
        println!("serial: {serial}");
    }
    if let Some(fw) = info.firmware {
        println!("firmware: {fw}");
    }
    println!(
        "ROI: x={} y={} w={} h={}",
        cfg.roi.x, cfg.roi.y, cfg.roi.width, cfg.roi.height
    );
    println!("DEM masked pixels: {}", cfg.pixel_mask.masked_pixels.len());
    println!(
        "filters: STC={} trail={}",
        cfg.digital_filter.stc_enabled, cfg.digital_filter.trail_enabled
    );

    Ok(())
}

fn record(
    output: PathBuf,
    config_path: Option<PathBuf>,
    duration_s: Option<u64>,
    annotations: RecordingAnnotations,
) -> Result<()> {
    let cfg = load_config(config_path.as_deref())?;

    let camera = Evk4Camera::open_imx636().context("failed opening EVK4")?;
    let info = camera.device_info();
    println!(
        "[{} / {}]",
        info.model,
        info.compatible.as_deref().unwrap_or_default()
    );
    println!("Recording -> {}", output.display());

    let mut options = PipelineOptions::new(&output);
    options.metadata =
        Some(RecordingMetadata::from_context(&info, &cfg).with_annotations(annotations));
    let controller = spawn_pipeline(camera, Evt3CorePreviewDecoder::default(), cfg, options)
        .context("failed starting streaming pipeline")?;

    let running = Arc::new(AtomicBool::new(true));
    {
        let running_flag = Arc::clone(&running);
        ctrlc::set_handler(move || {
            running_flag.store(false, Ordering::Relaxed);
        })
        .context("failed installing Ctrl+C handler")?;
    }

    let start = Instant::now();
    let mut fatal_error = None;
    while running.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_secs(1));
        if let Some(err) = controller.try_recv_error() {
            fatal_error = Some(err);
            break;
        }

        let stats = controller.stats_snapshot();
        println!(
            "  {:.1} Mev/s | {:.1} MB/s | {} elapsed | {:.2} GB written",
            stats.mev_per_s,
            stats.mb_per_s,
            fmt_elapsed(stats.elapsed_s),
            stats.bytes_total as f64 / (1024.0 * 1024.0 * 1024.0)
        );

        if let Some(max_s) = duration_s {
            if start.elapsed().as_secs() >= max_s {
                break;
            }
        }
    }

    controller.shutdown().context("pipeline shutdown failed")?;
    if let Some(err) = fatal_error {
        anyhow::bail!("pipeline failed: {err}");
    }
    println!("Stopped.");
    Ok(())
}

fn config_cmd(cmd: ConfigCommand) -> Result<()> {
    match cmd {
        ConfigCommand::Show { config } => {
            let cfg = load_config(config.as_deref())?;
            let encoded = toml::to_string_pretty(&cfg)?;
            println!("{encoded}");
        }
        ConfigCommand::SetBias { key, value, config } => {
            let path = config.unwrap_or_else(default_config_path);
            let mut cfg = load_config(Some(&path))?;
            match key.as_str() {
                "diff_on" => cfg.biases.diff_on = value,
                "diff_off" => cfg.biases.diff_off = value,
                "fo" => cfg.biases.fo = value,
                "hpf" => cfg.biases.hpf = value,
                "refr" => cfg.biases.refr = value,
                other => anyhow::bail!("unknown bias key '{other}'"),
            }
            cfg.save_to_path(&path)?;
            println!("updated {} in {}", key, path.display());
        }
        ConfigCommand::SetRoi {
            x,
            y,
            width,
            height,
            config,
        } => {
            let path = config.unwrap_or_else(default_config_path);
            let mut cfg = load_config(Some(&path))?;
            cfg.roi = RoiConfig {
                x,
                y,
                width,
                height,
            };
            cfg.validate(1280, 720)?;
            cfg.save_to_path(&path)?;
            println!("updated ROI in {}", path.display());
        }
        ConfigCommand::SetMask { x, y, config } => {
            let path = config.unwrap_or_else(default_config_path);
            let mut cfg = load_config(Some(&path))?;
            cfg.pixel_mask.masked_pixels.push((x, y));
            cfg.validate(1280, 720)?;
            cfg.save_to_path(&path)?;
            println!("added masked pixel ({x},{y}) in {}", path.display());
        }
    }
    Ok(())
}

fn load_config(path: Option<&Path>) -> Result<CameraConfig> {
    match path {
        Some(path) if path.exists() => {
            CameraConfig::load_from_path(path).context("failed reading config TOML")
        }
        _ => Ok(CameraConfig::default()),
    }
}

fn default_config_path() -> PathBuf {
    PathBuf::from("augur.toml")
}

fn fmt_elapsed(seconds: f64) -> String {
    let total = seconds as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    format!("{h:02}:{m:02}:{s:02}")
}
