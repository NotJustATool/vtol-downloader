use clap::Parser;
use fs_extra::dir::CopyOptions;
use std::error::Error;
use std::fs;
use std::io::{stdin, stdout, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;
use steamworks::{DownloadItemResult, PersonaStateChange, PublishedFileId};
use tokio::sync::Notify;
use walkdir::WalkDir;
use yansi::Color;

/// Downloads and decodes a workshop file from the Steam Workshop.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// The ID of the workshop file to download
    ///
    /// You can find this as the last number in the url for the workshop file.
    /// E.g.: https://steamcommunity.com/sharedfiles/filedetails/?id=2785198049
    /// where 2785198049 is the file ID.
    #[arg(short, long)]
    workshop_id: u64,

    /// Where to place the decoded files
    #[arg(short, long)]
    output_folder: PathBuf,

    /// Do not delete the encoded files
    #[arg(short = 'P', long)]
    preserve_encoded: bool,
}

fn get_confirmation(to_confirm: &str, default: bool) -> bool {
    let prompt = match default {
        true => "(Y/n)",
        false => "y/N",
    };
    print!("{to_confirm} {prompt}: ");
    stdout().flush().unwrap();
    let mut buf = String::new();
    stdin().read_line(&mut buf).unwrap();

    match buf.to_lowercase().trim() {
        "y" | "yes" => true,
        "n" | "no" => false,
        _ => default,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    // exit if not running on Windows. might not need
    #[cfg(not(windows))]
    {
        println!("This program is only for Windows.");
        return Ok(());
    }
    let args = Args::parse();

    // there's got to be a better way to save styles
    let error = yansi::Style::new(Color::Red);
    let success = yansi::Style::new(Color::Green);
    let title = yansi::Style::new(Color::White).bold().underline();

    let (client, single) = steamworks::Client::init_app(667970)?;

    // spawn a thread to process Steamworks callbacks
    tokio::spawn(async move {
        loop {
            single.run_callbacks();
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    });

    println!("{}", title.paint("VTOL VR Downloader"));
    println!("Getting info for {}...", args.workshop_id);
    println!();

    let file_id = PublishedFileId::from(args.workshop_id);

    // a mess of code to avoid callback hell
    let (tx, rx) = tokio::sync::oneshot::channel();
    let Ok(query) = client.ugc()
        .query_item(file_id)
        else {
            println!("{}", error.paint("Failed to get file details."));
            return Ok(());
        };
    query.fetch(|results| {
        tx.send(results.unwrap().get(0).unwrap()).unwrap();
    });

    let result = rx.await?;

    // another mess of code to get the author's username
    let notify = Arc::new(Notify::new());
    let notified = notify.clone();
    let cb = client.register_callback(move |change: PersonaStateChange| {
        if change.steam_id != result.owner {
            return;
        }
        notify.notify_one();
    });

    let not_cached = client
        .friends()
        .request_user_information(result.owner, true);
    if not_cached {
        notified.notified().await;
    }
    drop(cb); // unregister the callback

    println!(
        "Name: {}\nAuthor: {}\nDesc. (first line): {}\nUpvotes: {}\nDownvotes: {}",
        result.title,
        client.friends().get_friend(result.owner).name(),
        result.description.split_once('\n').unwrap().0,
        result.num_upvotes,
        result.num_downvotes,
    );
    println!();

    let confirmed = get_confirmation(
        &format!(
            "Download this file? ({:.2}MiB)",
            result.file_size as f32 / 1024.0 / 1024.0 // calculate file size in Mebibytes
        ),
        true,
    );
    if !confirmed {
        println!("{}", error.paint("Cancelled."));
        return Ok(());
    }

    println!("{}", success.paint("Starting downloaded..."));

    let notify = Arc::new(Notify::new());
    let notified = notify.clone();
    let cb = client.register_callback(move |result: DownloadItemResult| {
        if result.published_file_id != file_id {
            return;
        }
        notify.notify_one();
    });

    let started = client.ugc().download_item(file_id, true);
    if !started {
        println!(
            "{}",
            error.paint("Failed to start download. (Are you logged in to Steam?)")
        );
        return Ok(());
    }
    notified.notified().await;
    drop(cb);

    let Some(info) = client.ugc().item_install_info(file_id) else {
        println!(
            "{}",
            error.paint("Failed to retrieve downloaded file info. It may not have downloaded properly.")
        );
        return Ok(());
    };
    let download_folder = Path::new(&info.folder);
    println!("{}", success.paint("Successfully downloaded file."));

    if !args.output_folder.exists() {
        fs::create_dir_all(&args.output_folder)?;
    }
    let output_path = args.output_folder.canonicalize()?;

    let mut options = CopyOptions::new();
    options.content_only = true;
    fs_extra::dir::copy(download_folder, &output_path, &options)?;

    for entry in WalkDir::new(&output_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| {
            e.path()
                .extension()
                .unwrap()
                .to_string_lossy()
                .ends_with('b') // horrible code
        })
    {
        println!(
            "Decoding {}...",
            entry.path().file_name().unwrap().to_string_lossy()
        );
        let bytes: Vec<u8> = fs::read(entry.path())?
            .iter()
            .map(|b| b.wrapping_sub(88))
            .collect();
        fs::write(
            entry.path().to_string_lossy().strip_suffix('b').unwrap(), // more horrible code
            bytes,
        )?;
        if !args.preserve_encoded {
            fs::remove_file(entry.path())?;
        }
    }

    let mut xml_file = output_path.clone();
    xml_file.push("WorkshopItemInfo.xml");
    fs::remove_file(xml_file)?;

    println!(
        "{}",
        success.paint(format!(
            "Completed! The decoded files are in `{}`.",
            output_path
                .display()
                .to_string()
                .strip_prefix("\\\\?\\")
                .unwrap_or(&output_path.display().to_string()) // evil code
        ))
    );

    Ok(())
}
