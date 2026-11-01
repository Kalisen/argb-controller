//! Spike for the Cloud Nine C959 ErgoTKL: drives the keyboard from the
//! console with the protocol of `argb-backend-sonix`, for experiments the
//! service does not need (raw commands, arbitrary effect ids, per-key
//! colors). See `docs/phase2-1-peripherals.md`.

use anyhow::{Context, bail};
use argb_backend_sonix::model::SONIX_VENDOR_ID;
use argb_backend_sonix::protocol::{
    BrightnessLevel, Command, EffectRecord, LightingBlock, REPORT_LEN, Report, Speed, acknowledged,
};
use argb_backend_sonix::transport::{HidLink, Link, attached_keyboards, write_section};
use argb_domain::Rgb;
use clap::{Parser, Subcommand};
use hidapi::HidApi;

#[derive(Parser)]
#[command(about = "Sonix keyboard protocol spike")]
struct Cli {
    /// Substring of the keyboard's reported name, e.g. "C959".
    #[arg(long, default_value = "C959")]
    keyboard: String,
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// List every HID collection of Sonix devices, and the supported keyboards among them.
    List,
    /// Run a built-in effect: writes the whole lighting block with this record active.
    Light {
        /// Effect id: 1 static color, 2 breathing, up to 19 in the application's order.
        #[arg(long, default_value_t = 1)]
        mode: u8,
        /// Color as RRGGBB.
        #[arg(long, default_value = "40e0d0")]
        color: String,
        /// 1 (the application's "Dim") to 6 ("High").
        #[arg(long, default_value_t = 6)]
        brightness: u8,
        /// 5 to 9 as the application sends them.
        #[arg(long, default_value_t = 6)]
        speed: u8,
        /// 0 for the chosen color, 1 for the built-in rainbow.
        #[arg(long, default_value_t = 0)]
        rainbow: u8,
        /// 0 normal, 2 inverted.
        #[arg(long, default_value_t = 0)]
        direction: u8,
    },
    /// Switch to the per-key mode with every key set to one color.
    Fill {
        /// Color as RRGGBB.
        #[arg(long, default_value = "40e0d0")]
        color: String,
        /// 1 to 6.
        #[arg(long, default_value_t = 5)]
        brightness: u8,
    },
    /// Write the lighting block with no active record, which turns the LEDs off.
    Off,
    /// Write raw data pages (hex bytes each) into a section with the full session.
    Session {
        /// Section command byte, e.g. 13.
        #[arg(long)]
        section: String,
        /// One hex string per page, e.g. "01 ff ff ff 00 00 00 00 00 05 03 00 00 00 aa 55".
        pages: Vec<String>,
    },
    /// Send one raw payload (hex bytes after the report id) and print the echo.
    Raw {
        /// Hex bytes, e.g. "04 18".
        bytes: String,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Action::List => list(),
        Action::Light {
            mode,
            color,
            brightness,
            speed,
            rainbow,
            direction,
        } => {
            let record = EffectRecord {
                mode,
                color: Rgb::from_hex(&color)?,
                rainbow,
                brightness: BrightnessLevel::from_level(brightness),
                speed: Speed::from_value(speed),
                direction,
            };
            write_block(&cli.keyboard, &LightingBlock::running(record))?;
            println!(
                "lighting written: mode {mode} color {color} brightness {brightness} speed {speed}"
            );
            Ok(())
        }
        Action::Fill { color, brightness } => {
            let block = LightingBlock::per_key(
                Rgb::from_hex(&color)?,
                BrightnessLevel::from_level(brightness),
            );
            write_block(&cli.keyboard, &block)?;
            println!("per-key table written: every key {color}, brightness {brightness}");
            Ok(())
        }
        Action::Off => {
            write_block(&cli.keyboard, &LightingBlock::off())?;
            println!("lighting off");
            Ok(())
        }
        Action::Session { section, pages } => {
            let section = parse_section(&section)?;
            let reports = pages
                .iter()
                .map(|text| parse_report(text))
                .collect::<anyhow::Result<Vec<Report>>>()?;
            let mut link = HidLink::new(&cli.keyboard);
            write_section(&mut link, section, &reports)?;
            println!(
                "session written: section {:02x}, {} page(s)",
                section as u8,
                reports.len()
            );
            Ok(())
        }
        Action::Raw { bytes } => {
            let report = parse_report(&bytes)?;
            let mut link = HidLink::new(&cli.keyboard);
            link.open()?;
            link.send(&report)?;
            let echo = link.receive()?;
            println!(
                "echo: {} (acknowledged: {})",
                hex(&echo[1..]),
                acknowledged(&echo)
            );
            Ok(())
        }
    }
}

fn list() -> anyhow::Result<()> {
    let api = HidApi::new().context("initializing hidapi")?;
    for info in api
        .device_list()
        .filter(|info| info.vendor_id() == SONIX_VENDOR_ID)
    {
        println!(
            "{:04x}:{:04x} interface {} usage {:04x}:{:04x} product {:?}",
            info.vendor_id(),
            info.product_id(),
            info.interface_number(),
            info.usage_page(),
            info.usage(),
            info.product_string().unwrap_or("")
        );
    }
    println!();
    for keyboard in attached_keyboards()? {
        println!("supported: {}", keyboard.model.reported_name());
    }
    Ok(())
}

fn write_block(keyboard: &str, block: &LightingBlock) -> anyhow::Result<()> {
    let mut link = HidLink::new(keyboard);
    write_section(&mut link, Command::Lighting, &block.pages())?;
    Ok(())
}

fn parse_section(text: &str) -> anyhow::Result<Command> {
    let byte = u8::from_str_radix(text, 16).context("section hex")?;
    let known = [
        Command::Lighting,
        Command::Begin,
        Command::Commit,
        Command::Apply,
    ];
    known
        .into_iter()
        .find(|section| *section as u8 == byte)
        .with_context(|| format!("section {byte:02x} is not one the backend names; use raw"))
}

fn parse_report(text: &str) -> anyhow::Result<Report> {
    let mut report = [0u8; REPORT_LEN];
    let bytes: Vec<&str> = text.split_whitespace().collect();
    if bytes.len() > REPORT_LEN - 1 {
        bail!("a payload is at most {} bytes", REPORT_LEN - 1);
    }
    for (slot, byte) in report[1..].iter_mut().zip(bytes) {
        *slot = u8::from_str_radix(byte, 16).with_context(|| format!("hex byte {byte}"))?;
    }
    Ok(report)
}

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_payload_fills_the_report_after_the_id() {
        let report = parse_report("04 18").unwrap();
        assert_eq!(&report[..4], &[0x00, 0x04, 0x18, 0x00]);
        assert!(parse_report("zz").is_err());
    }

    #[test]
    fn only_named_sections_are_accepted() {
        assert_eq!(parse_section("13").unwrap(), Command::Lighting);
        assert!(parse_section("23").is_err());
    }
}
