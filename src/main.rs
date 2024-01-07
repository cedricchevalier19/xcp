/*
 * Copyright © 2018, Steve Smith <tarkasteve@gmail.com>
 *
 * This program is free software: you can redistribute it and/or
 * modify it under the terms of the GNU General Public License version
 * 3 as published by the Free Software Foundation.
 *
 * This program is distributed in the hope that it will be useful, but
 * WITHOUT ANY WARRANTY; without even the implied warranty of
 * MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the GNU
 * General Public License for more details.
 *
 * You should have received a copy of the GNU General Public License
 * along with this program.  If not, see <https://www.gnu.org/licenses/>.
 */

mod drivers;
mod errors;
mod operations;
mod options;
mod progress;
mod utils;

use std::path::PathBuf;
use std::sync::Arc;

use crossbeam_channel as cbc;
use libfs::is_same_file;
use log::{error, info};
use operations::{StatSender, StatusUpdate};
use options::Opts;
use simplelog::{ColorChoice, Config, LevelFilter, SimpleLogger, TermLogger, TerminalMode};

use crate::drivers::load_driver;
use crate::errors::{Result, XcpError};

fn init_logging(opts: &Opts) -> Result<()> {
    let log_level = match opts.verbose {
        0 => LevelFilter::Warn,
        1 => LevelFilter::Info,
        2 => LevelFilter::Debug,
        _ => LevelFilter::Trace,
    };

    TermLogger::init(
        log_level,
        Config::default(),
        TerminalMode::Mixed,
        ColorChoice::Auto,
    ).or_else(|_| SimpleLogger::init(log_level, Config::default()))?;

    Ok(())
}

fn main() -> Result<()> {
    let opts = Arc::new(options::parse_args()?);
    init_logging(&opts)?;

    let (dest, source_patterns) = opts
        .paths
        .split_last()
        .ok_or(XcpError::InvalidArguments("Insufficient arguments".to_string()))
        .map(|(d, s)| (PathBuf::from(d), s))?;

    // Do this check before expansion otherwise it could result in
    // unexpected behaviour when the a glob expands to a single file.
    if source_patterns.len() > 1 && !dest.is_dir() {
        return Err(XcpError::InvalidDestination(
            "Multiple sources and destination is not a directory.",
        )
        .into());
    }

    let sources = options::expand_sources(source_patterns, &opts)?;
    if sources.is_empty() {
        return Err(XcpError::InvalidSource("No source files found.").into());

    }

    let pb = progress::create_bar(&opts, 0)?;
    let (stat_tx, stat_rx) = cbc::unbounded();
    let stats = StatSender::new(stat_tx, &opts);

    let driver = load_driver(&opts)?;

    if sources.len() == 1 && dest.is_file() {
        let source = &sources[0];

        // Special case; attemping to rename/overwrite existing file.
        if opts.no_clobber {
            return Err(XcpError::DestinationExists(
                "Destination file exists and --no-clobber is set.",
                dest,
            )
            .into());
        }

        // Special case: Attempt to overwrite a file with
        // itself. Always disallow for now.
        if is_same_file(&source, &dest)? {
            return Err(XcpError::DestinationExists(
                "Source and destination is the same file.",
                dest,
            )
            .into());
        }

        info!("Copying file {:?} to {:?}", source, dest);
        driver.copy_single(source, &dest, stats)?;

    } else {
        // Sanity-check all sources up-front
        for source in &sources {
            info!("Copying source {:?} to {:?}", source, dest);
            if !source.exists() {
                return Err(XcpError::InvalidSource("Source does not exist.").into());
            }

            if source.is_dir() && !opts.recursive {
                return Err(XcpError::InvalidSource(
                    "Source is directory and --recursive not specified.",
                )
                .into());
            }

            if source == &dest {
                return Err(XcpError::InvalidSource("Cannot copy a directory into itself").into());
            }

            if dest.exists() && !dest.is_dir() {
                return Err(XcpError::InvalidDestination(
                    "Source is directory but target exists and is not a directory",
                )
                .into());
            }
        }

        driver.copy_all(sources, &dest, stats)?;
    }

    // Gather the results as we go; our end of the channel has been
    // moved to the driver call and will end when drained.
    for stat in stat_rx {
        match stat {
            StatusUpdate::Copied(v) => pb.inc(v),
            StatusUpdate::Size(v) => pb.inc_size(v),
            StatusUpdate::Error(e) => {
                // FIXME: Optional continue?
                error!("Received error: {}", e);
                return Err(e.into());
            }
        }
    }

    info!("Copy complete");
    pb.end();

    Ok(())
}
