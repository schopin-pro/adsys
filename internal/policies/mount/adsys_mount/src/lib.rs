use gio::{
    self,
    traits::{FileExt, MountOperationExt},
};
use glib::ObjectExt;
use log::{debug, error, warn};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    sync::{Arc, Mutex},
};

mod errors;
pub use errors::AdsysMountError;

/// Represents a mount point read from the mounts file.
#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct MountEntry {
    mount_path: String,
    is_anonymous: bool,
}

/// Struct representing the message that is to be passed in the glib channel.
pub struct Msg {
    path: String,
    status: MountResult,
}

pub type MountResult = Result<MountStatus, glib::Error>;

/// Represents the status returned by a mount attempt.
#[derive(Debug)]
#[non_exhaustive]
pub enum MountStatus {
    Done,
    Asked,
}

struct MountError {
    path: String,
    error: glib::Error,
}

fn user_mount_cb(
    msg: Msg,
    errors: &Mutex<Vec<MountError>>,
    main_loop: &glib::MainLoop,
    mounts_left: &mut usize,
) -> glib::Continue {
    let Msg { path, status } = msg;
    match status {
        Err(error) => {
            warn!("Failed when mounting {}", path);
            errors.lock().unwrap().push(MountError { path, error });
        }
        Ok(MountStatus::Done) => debug!("Mounting of {} was successful", path),
        _ => error!("Unexpected return status: {:?}", status),
    };
    *mounts_left -= 1;

    // Ends the main loop if there are no more mounts left.
    if *mounts_left == 0 {
        main_loop.quit();
    }
    glib::Continue(*mounts_left != 0)
}

pub fn handle_user_mounts(mounts_file: &str) -> Result<(), AdsysMountError> {
    debug!("Mounting entries listed in {}", mounts_file);

    let parsed_entries = parse_entries(mounts_file).map_err(|e| {
        error!("Error when parsing entries: {}", e);
        AdsysMountError::ParseError
    })?;

    if parsed_entries.is_empty() {
        return Ok(());
    }

    // Setting up the channel used for communication between the mount operations and the main function.
    let g_ctx = glib::MainContext::default();
    let (tx, rx) = glib::MainContext::channel(glib::PRIORITY_DEFAULT);

    // Grabs the ammount of mounts to be done before passing the ownership of parsed_entries.
    let mut mounts_left = parsed_entries.len();

    for entry in parsed_entries {
        handle_mount(entry, tx.clone());
    }

    // Sets the main loop glib to be used by the mounts
    let g_loop = glib::MainLoop::new(Some(&g_ctx), false);

    // Creates a mutex to handle the exit status
    let errors = Arc::new(Mutex::new(Vec::new()));

    // Attaches the receiver to the main context, along with a closure that is called everytime there is a new message in the channel.
    {
        // Clone shared data for closure capture.
        let errors = errors.clone();
        let g_loop = g_loop.clone();
        rx.attach(Some(&g_ctx), move |msg| {
            user_mount_cb(msg, &errors, &g_loop, &mut mounts_left)
        });
    }

    g_loop.run();

    // Evaluates the arc content to check if at least one operation failed.
    let errors = errors.lock().unwrap();
    if errors.is_empty() {
        return Ok(());
    }

    for MountError { path, error } in errors.iter() {
        warn!("Mount process for {} failed: {}", path, error);
    }

    // Ensures that the function will not error out if the location was already mounted.
    if errors
        .iter()
        .any(|MountError { error, .. }| !error.matches(gio::IOErrorEnum::AlreadyMounted))
    {
        Ok(())
    } else {
        Err(AdsysMountError::MountError)
    }
}

/// Reads the file and parses the mount points listed in it.
fn parse_entries(path: &str) -> Result<Vec<MountEntry>, std::io::Error> {
    debug!("Parsing file {} content", path);

    let mut parsed_entries: Vec<MountEntry> = Vec::new();

    // The ? operator tries to unwrap the result and, if there is an error, returns it to the caller of this function.
    let content = fs::read_to_string(path)?;

    for p in content.lines() {
        if p.is_empty() {
            continue;
        }

        parsed_entries.push(match p.strip_prefix("[anonymous]") {
            Some(s) => MountEntry {
                mount_path: s.to_string(),
                is_anonymous: true,
            },
            None => MountEntry {
                mount_path: p.to_string(),
                is_anonymous: false,
            },
        });
    }

    Ok(parsed_entries)
}

/// Handles the mount operation to mount the specified entry.
fn handle_mount(entry: MountEntry, tx: glib::Sender<Msg>) {
    debug!("Mounting entry {}", entry.mount_path);

    let f = gio::File::for_uri(&entry.mount_path);

    let mount_op = gio::MountOperation::new();

    if entry.is_anonymous {
        debug!("Anonymous mount requested for {}", entry.mount_path);
        mount_op.set_anonymous(true);
    }

    mount_op.connect_ask_password(ask_password_cb);

    // Callback invoked by gio after setting up the mount.
    let mount_handled_cb = move |r: Result<(), glib::Error>| {
        let msg = Msg {
            path: entry.mount_path,
            status: r.map(|_| MountStatus::Done),
        };
        if let Err(e) = tx.send(msg) {
            error!("Failed to send message in the channel: {}", e)
        };
    };

    f.mount_enclosing_volume(
        gio::MountMountFlags::NONE,
        Some(&mount_op),
        gio::Cancellable::NONE,
        mount_handled_cb,
    );
}

/// Callback that is invoked by gio when prompted for password.
fn ask_password_cb(
    mount_op: &gio::MountOperation,
    _: &str,
    _: &str,
    _: &str,
    flags: gio::AskPasswordFlags,
) {
    if mount_op.is_anonymous() && flags.contains(gio::AskPasswordFlags::ANONYMOUS_SUPPORTED) {
        // Unsafe block is needed for data and set_data implementations in glib.
        unsafe {
            if let Some(data) = mount_op.data::<MountResult>("state") {
                // Ensures that we only try anonymous access once.
                if let Ok(MountStatus::Asked) = *(data.as_ptr()) {
                    warn!("Anonymous access denied.");
                    mount_op.reply(gio::MountOperationResult::Aborted);
                }
            } else {
                debug!("Anonymous is supported by the provider.");
                mount_op.set_data("state", Ok(MountStatus::Asked) as MountResult);
                mount_op.reply(gio::MountOperationResult::Handled);
            }
        }
        return;
    }

    // Checks if the machine has a kerberos ticket defined.
    if std::env::var("KRB5CCNAME").is_ok() {
        debug!("Kerberos ticket found on the machine.");
        mount_op.reply(gio::MountOperationResult::Handled);
        return;
    }

    warn!("Kerberos ticket not available on the machine.");
    mount_op.reply(gio::MountOperationResult::Aborted);
}

mod test;
mod test_utils;
