//! Phase 1 command line front end.
//!
//!     gdeck list              show the keyboard's HID collections
//!     gdeck watch             print G key events until Ctrl+C
//!     gdeck watch 30          print G key events for 30 seconds, then stop cleanly

use std::io::{self, Write};
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant};

use gdeck::{device, security, store, Event, GKey, Policy, Reader, Sid, G_KEY_COUNT};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("watch");

    println!("gdeck: read only. No HID write function is linked into this binary.");
    println!();

    match cmd {
        "list" => cmd_list(),
        "watch" => cmd_watch(args.get(2).and_then(|s| s.parse::<u64>().ok())),
        "security" => cmd_security(
            args.get(2).map(|s| s.as_str()).unwrap_or("show"),
            args.get(3).map(|s| s.as_str()),
        ),
        "config" => cmd_config(args.get(2).map(|s| s.as_str()).unwrap_or("check")),
        "audit" => cmd_audit(args.get(2).and_then(|s| s.parse::<usize>().ok()).unwrap_or(25)),
        "press" => cmd_press(args.get(2).map(|s| s.as_str()).unwrap_or("")),
        "run" => cmd_run(args.get(2).and_then(|s| s.parse::<u64>().ok())),
        other => {
            println!("unknown subcommand: {}", other);
            usage();
        }
    }
}

fn usage() {
    println!("usage:");
    println!("  gdeck list                       the keyboard's HID collections");
    println!("  gdeck watch [seconds]            print G key events");
    println!("  gdeck security show              store path, descriptor and ACEs");
    println!("  gdeck security apply [admins]    create the store and harden it");
    println!("  gdeck security verify [admins]   compare what is on disk to the policy");
    println!("  gdeck config path                where the config lives");
    println!("  gdeck config init                create an empty config and secure it");
    println!("  gdeck config check               parse the config and report");
    println!("  gdeck config integrity           has the config changed outside the app?");
    println!("  gdeck config seal                accept the config on disk as current");
    println!("  gdeck audit [count]              recent entries from the audit log");
    println!("  gdeck press G1                   carry out one key's action now");
    println!("  gdeck run [seconds]              watch the keyboard and act on presses");
    println!();
    println!("`admins` additionally grants BUILTIN\\Administrators. Off by default:");
    println!("it does not lock administrators out either way, since they can take");
    println!("ownership, but leaving it out means they have to, which is detectable.");
}

fn print_aces(label: &str, sddl: &str) {
    let aces = security::parse_aces(sddl);
    if aces.is_empty() {
        println!("{}: no ACEs found", label);
        return;
    }
    println!("{}:", label);
    for (n, ace) in aces.iter().enumerate() {
        let who = ace
            .trustee_name
            .clone()
            .unwrap_or_else(|| ace.trustee.clone());
        println!("  {}. {:<6} {}", n + 1, ace.kind, who);
        println!("     rights      : {}", ace.rights);
        println!("     inheritance : {}", ace.flags);
        println!("     raw         : {}", ace.raw);
    }
}

fn report_exposure(sddl: &str) {
    let exposed = security::forbidden_present(sddl);
    if exposed.is_empty() {
        println!("No forbidden trustee is present.");
    } else {
        for why in exposed {
            println!("  EXPOSED: {}", why);
        }
    }
}

/// Resolve the lockdown policy for the current account.
fn current_policy() -> Option<Policy> {
    match Sid::current_user() {
        Ok(sid) => Some(Policy::lockdown(sid)),
        Err(err) => {
            eprintln!("could not resolve your SID: {}", err);
            None
        }
    }
}

/// Load the config, refusing if it changed outside the app.
///
/// This is the gate the whole of Phase 2 exists to make possible. The config
/// says what gets executed, so an unexplained change is a reason to stop, not
/// a reason to carry on and hope.
fn load_checked_config() -> Option<gdeck::Config> {
    use gdeck::integrity::{changed_only, check, DiffLine, Integrity};

    let text = read_config_text()?;

    match check(&text) {
        Ok(Integrity::Intact) => {}
        Ok(Integrity::NotSealed) => {
            println!("WARNING: this config has never been sealed, so a change made");
            println!("         outside the app cannot be detected. `gdeck config seal`");
            println!("         makes the current file the accepted baseline.");
            println!();
        }
        Ok(Integrity::Modified { diff }) => {
            println!("REFUSING TO RUN: the config changed outside the app.");
            println!();
            for line in changed_only(&diff) {
                match line {
                    DiffLine::Removed(text) => println!("  - {}", text),
                    DiffLine::Added(text) => println!("  + {}", text),
                    DiffLine::Same(_) => {}
                }
            }
            println!();
            println!("  Nothing has been run. Review the change above. If it was you,");
            println!("  `gdeck config seal` accepts it and this will start normally.");
            audit(
                None,
                "run",
                gdeck::audit::Outcome::Refused,
                "config changed outside the app",
            );
            return None;
        }
        Ok(Integrity::SealUnreadable { code }) => {
            println!("REFUSING TO RUN: a seal exists but cannot be opened (win32 {}).", code);
            println!("  It belongs to a different account, or it is corrupt.");
            audit(
                None,
                "run",
                gdeck::audit::Outcome::Refused,
                "seal unreadable",
            );
            return None;
        }
        Err(err) => {
            eprintln!("{}", err);
            return None;
        }
    }

    match gdeck::config::parse(&text) {
        Ok(config) => Some(config),
        Err(err) => {
            println!("The config was rejected, so nothing is bound.");
            println!("  {}", err);
            None
        }
    }
}

/// Run one slot's action and record the result.
fn fire(key: Option<GKey>, slot: &gdeck::Slot, context: &gdeck::actions::Context<'_>) {
    use gdeck::actions::Performed;
    use gdeck::audit::Outcome;

    let (action, detail) = gdeck::actions::describe(slot);
    let label = key.map(|k| k.to_string()).unwrap_or_else(|| "-".to_string());

    match gdeck::actions::perform(slot, context) {
        Ok(Performed::Nothing) => println!("  {} is unassigned", label),
        Ok(Performed::Launched { process_id }) => {
            println!("  {} launched {} (pid {})", label, slot.label, process_id);
            audit(key, action, Outcome::Started, &detail);
        }
        Ok(Performed::Focused) => {
            println!("  {} focused the running {}", label, slot.label);
            audit(key, action, Outcome::Started, &format!("focused {}", detail));
        }
        Ok(Performed::Opened) => {
            println!("  {} opened {}", label, detail);
            audit(key, action, Outcome::Started, &detail);
        }
        Ok(Performed::ShowNote { path }) => {
            println!("  {} note ready at {}", label, path.display());
            audit(key, action, Outcome::Started, &detail);
        }
        Ok(Performed::RanShell(run)) => {
            match run.exit_code {
                Some(0) => println!("  {} ran {} and it succeeded (exit 0)", label, slot.label),
                Some(code) => println!("  {} ran {} and it FAILED (exit {})", label, slot.label, code),
                None => println!(
                    "  {} ran {} (pid {}), still running after the wait",
                    label, slot.label, run.process_id
                ),
            }
            if !run.output.is_empty() {
                for line in run.output.lines() {
                    println!("       | {}", line);
                }
            }
            let outcome = match run.exit_code {
                Some(0) | None => Outcome::Started,
                Some(_) => Outcome::Failed,
            };
            let note = match run.exit_code {
                Some(code) => format!("{} (exit {})", detail, code),
                None => format!("{} (still running)", detail),
            };
            audit(key, action, outcome, &note);
        }
        Err(gdeck::Error::Refused { reason }) => {
            println!("  {} REFUSED: {}", label, reason);
            audit(key, action, Outcome::Refused, &format!("{}: {}", detail, reason));
        }
        Err(err) => {
            println!("  {} FAILED: {}", label, err);
            audit(key, action, Outcome::Failed, &format!("{}: {}", detail, err));
        }
    }
}

fn cmd_press(which: &str) {
    let number: u8 = match which
        .trim()
        .trim_start_matches(['G', 'g'])
        .parse::<u8>()
        .ok()
        .filter(|n| (1..=18).contains(n))
    {
        Some(number) => number,
        None => {
            println!("Name a key from G1 to G18, for example: gdeck press G1");
            return;
        }
    };
    let key = GKey::from_index(number as usize - 1).expect("checked range");

    let config = match load_checked_config() {
        Some(config) => config,
        None => return,
    };
    let policy = match current_policy() {
        Some(policy) => policy,
        None => return,
    };

    let slot = match config.slot(key) {
        Some(slot) => slot,
        None => {
            println!("{} is not assigned to anything.", key);
            return;
        }
    };

    println!("{} -> {} ({})", key, slot.label, slot.action.type_name());
    let context = gdeck::actions::Context {
        shell_enabled: config.shell_enabled,
        policy: &policy,
    };
    fire(Some(key), slot, &context);
}

fn cmd_run(seconds: Option<u64>) {
    let config = match load_checked_config() {
        Some(config) => config,
        None => return,
    };
    let policy = match current_policy() {
        Some(policy) => policy,
        None => return,
    };

    println!(
        "{} of 18 keys assigned. Shell actions {}.",
        config.assigned_count(),
        if config.shell_enabled { "ENABLED" } else { "disabled" }
    );

    let (mut reader, events) = match Reader::start() {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("could not start the reader: {}", err);
            return;
        }
    };

    match seconds {
        Some(s) => println!("Running for {} seconds once you begin.", s),
        None => println!("Running. Press Ctrl+C to stop."),
    }

    if seconds.is_some() {
        println!();
        print!("Press Enter to start. ");
        let _ = io::stdout().flush();
        let mut ready = String::new();
        let _ = io::stdin().read_line(&mut ready);
    }
    println!();

    let context = gdeck::actions::Context {
        shell_enabled: config.shell_enabled,
        policy: &policy,
    };

    let started = Instant::now();
    let deadline = seconds.map(|s| started + Duration::from_secs(s));

    loop {
        let event = match deadline {
            Some(end) => {
                let now = Instant::now();
                if now >= end {
                    break;
                }
                match events.recv_timeout(end - now) {
                    Ok(event) => event,
                    Err(_) => break,
                }
            }
            None => match events.recv() {
                Ok(event) => event,
                Err(_) => break,
            },
        };

        match event {
            Event::Connected => println!("keyboard connected"),
            Event::Disconnected => println!("keyboard disconnected"),
            Event::Released(_) => {}
            // Act on the press, not the release, so the response is immediate.
            Event::Pressed(key) => match config.slot(key) {
                Some(slot) => {
                    println!("{} {}", key, slot.label);
                    fire(Some(key), slot, &context);
                }
                None => println!("{} unassigned", key),
            },
        }
    }

    reader.stop();
    println!();
    println!("Stopped cleanly after {:.1}s.", started.elapsed().as_secs_f64());
}

fn cmd_audit(count: usize) {
    match gdeck::audit::tail(count) {
        Ok(entries) if entries.is_empty() => println!("The audit log is empty."),
        Ok(entries) => {
            println!("last {} entr{}:", entries.len(), if entries.len() == 1 { "y" } else { "ies" });
            println!();
            println!("{:<22} {:<5} {:<9} {:<9} DETAIL", "WHEN (UTC)", "KEY", "ACTION", "OUTCOME");
            for entry in entries {
                let f: Vec<&str> = entry.split('\t').collect();
                if f.len() == 5 {
                    println!("{:<22} {:<5} {:<9} {:<9} {}", f[0], f[1], f[2], f[3], f[4]);
                } else {
                    println!("{}", entry);
                }
            }
        }
        Err(err) => eprintln!("{}", err),
    }
}

/// Write an audit entry, and say so loudly if it could not be written.
///
/// Never silently ignore this. A log that quietly stops recording is worse
/// than no log at all, because it still looks authoritative.
fn audit(key: Option<GKey>, action: &str, outcome: gdeck::audit::Outcome, detail: &str) {
    if let Err(err) = gdeck::audit::record(key, action, outcome, detail) {
        eprintln!();
        eprintln!("WARNING: the audit log could not be written: {}", err);
        eprintln!("         This entry is lost, so the log is now incomplete.");
    }
}

fn read_config_text() -> Option<String> {
    let path = match store::config_path() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("{}", err);
            return None;
        }
    };
    if !path.exists() {
        println!("There is no config yet. `gdeck config init` creates one.");
        return None;
    }
    match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(err) => {
            eprintln!("could not read the config: {}", err);
            None
        }
    }
}

fn cmd_config(action: &str) {
    let path = match store::config_path() {
        Ok(path) => path,
        Err(err) => {
            eprintln!("{}", err);
            return;
        }
    };

    println!("config : {}", path.display());
    println!("exists : {}", if path.exists() { "yes" } else { "no" });
    println!();

    match action {
        "path" => {}

        "init" => {
            if path.exists() {
                println!("It already exists. Refusing to overwrite it.");
                println!("`gdeck config check` parses what is there.");
                return;
            }
            let owner = match Sid::current_user() {
                Ok(sid) => sid,
                Err(err) => {
                    eprintln!("could not resolve your SID: {}", err);
                    return;
                }
            };
            let policy = Policy::lockdown(owner);
            match gdeck::config::save(&gdeck::Config::default(), &policy) {
                Ok(()) => {
                    println!("Created and secured.");
                    println!();
                    match security::read_sddl(&path) {
                        Ok(sddl) => {
                            println!("file descriptor: {}", sddl);
                            println!();
                            print_aces("file ACEs", &sddl);
                        }
                        Err(err) => println!("could not read the file descriptor: {}", err),
                    }
                }
                Err(err) => eprintln!("could not write the config: {}", err),
            }
        }

        "check" => match gdeck::config::load() {
            Ok(config) => {
                println!("PARSED OK");
                println!("  schema version : {}", config.version);
                println!("  shell actions  : {}", if config.shell_enabled { "ENABLED" } else { "disabled" });
                println!("  keys assigned  : {} of 18", config.assigned_count());
                if config.assigned_count() > 0 {
                    println!();
                    for key in GKey::all() {
                        if let Some(slot) = config.slot(key) {
                            println!("  {:<4} {:<10} {}", key.to_string(), slot.action.type_name(), slot.label);
                        }
                    }
                }
            }
            Err(err) => {
                println!("REJECTED");
                println!("  {}", err);
                println!();
                println!("Nothing was loaded. The parser refuses anything it does not");
                println!("fully understand rather than guessing, because this file");
                println!("decides what gets executed.");
            }
        },

        "integrity" => {
            use gdeck::integrity::{changed_only, check, DiffLine, Integrity};

            let text = match read_config_text() {
                Some(text) => text,
                None => return,
            };

            match check(&text) {
                Ok(Integrity::NotSealed) => {
                    println!("NOT SEALED");
                    println!("  Nothing to compare against yet. `gdeck config seal` takes the");
                    println!("  config as it stands now and makes it the accepted baseline.");
                }
                Ok(Integrity::Intact) => {
                    println!("INTACT");
                    println!("  Byte for byte what the app last wrote.");
                }
                Ok(Integrity::Modified { diff }) => {
                    println!("MODIFIED OUTSIDE THE APP");
                    println!();
                    for line in changed_only(&diff) {
                        match line {
                            DiffLine::Removed(text) => println!("  - {}", text),
                            DiffLine::Added(text) => println!("  + {}", text),
                            DiffLine::Same(_) => {}
                        }
                    }
                    println!();
                    println!("  minus was sealed, plus is on disk now.");
                    println!("  If you made these edits, `gdeck config seal` accepts them.");
                    audit(
                        None,
                        "integrity",
                        gdeck::audit::Outcome::Refused,
                        "config changed outside the app",
                    );
                }
                Ok(Integrity::SealUnreadable { code }) => {
                    println!("SEAL UNREADABLE (win32 error {})", code);
                    println!("  A seal is present but this account cannot open it. Either it");
                    println!("  was written by a different user, or it is corrupt.");
                }
                Err(err) => eprintln!("{}", err),
            }
        }

        "seal" => {
            let text = match read_config_text() {
                Some(text) => text,
                None => return,
            };

            // Never bless something that would be refused at load time.
            if let Err(err) = gdeck::config::parse(&text) {
                println!("Refusing to seal: the config does not parse.");
                println!("  {}", err);
                return;
            }

            let owner = match Sid::current_user() {
                Ok(sid) => sid,
                Err(err) => {
                    eprintln!("{}", err);
                    return;
                }
            };
            match gdeck::integrity::write_seal(&text, &Policy::lockdown(owner)) {
                Ok(()) => {
                    println!("Sealed. The config on disk is now the accepted baseline.");
                    audit(None, "seal", gdeck::audit::Outcome::Started, "config sealed");
                }
                Err(err) => eprintln!("{}", err),
            }
        }

        other => {
            println!("unknown config action: {}", other);
            println!();
            usage();
        }
    }
}

fn cmd_security(action: &str, modifier: Option<&str>) {
    let owner = match Sid::current_user() {
        Ok(sid) => sid,
        Err(err) => {
            eprintln!("could not resolve your SID: {}", err);
            return;
        }
    };

    let with_admins = modifier == Some("admins");
    let policy = if with_admins {
        Policy::with_administrators(owner.clone())
    } else {
        Policy::lockdown(owner.clone())
    };

    let dir = match store::store_dir() {
        Ok(dir) => dir,
        Err(err) => {
            eprintln!("{}", err);
            return;
        }
    };

    let name = owner
        .display_name()
        .map(|n| format!("  ({})", n))
        .unwrap_or_default();
    println!("account : {}{}", owner.as_str(), name);
    println!("store   : {}", dir.display());
    println!(
        "policy  : {}",
        if with_admins {
            "owner, SYSTEM and BUILTIN\\Administrators"
        } else {
            "owner and SYSTEM only (lockdown, the default)"
        }
    );
    println!("exists  : {}", if dir.exists() { "yes" } else { "no" });
    println!();

    let desired = policy.directory_sddl();

    match action {
        "show" => {
            println!("DESIRED descriptor");
            println!("  {}", desired);
            println!();
            print_aces("desired ACEs", &desired);

            if !dir.exists() {
                println!();
                println!("The store does not exist yet.");
                println!("`gdeck security apply` creates it and applies the descriptor above.");
                return;
            }

            match security::read_sddl(&dir) {
                Ok(current) => {
                    println!();
                    println!("ON DISK descriptor");
                    println!("  {}", current);
                    println!();
                    print_aces("on disk ACEs", &current);
                    println!();
                    report_exposure(&current);
                }
                Err(err) => println!("could not read the descriptor: {}", err),
            }
        }

        "apply" => {
            let secured = match store::secure_all(&policy) {
                Ok(secured) => secured,
                Err(err) => {
                    eprintln!("could not apply the policy: {}", err);
                    return;
                }
            };

            println!("Applied to {} object(s):", secured.len());
            for (path, kind) in &secured {
                println!(
                    "  {:<18} {}",
                    kind,
                    path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.to_string_lossy().to_string())
                );
            }
            println!();
            println!("Re-reading to confirm rather than assuming it took.");
            println!();

            let dir = match store::store_dir() {
                Ok(dir) => dir,
                Err(err) => {
                    eprintln!("{}", err);
                    return;
                }
            };

            match security::verify(&dir, &desired) {
                Ok(drift) if drift.matches => {
                    println!("CONFIRMED: on disk matches the policy.");
                    println!("  {}", drift.actual);
                    println!();
                    print_aces("live ACEs", &drift.actual);
                    println!();
                    report_exposure(&drift.actual);
                }
                Ok(drift) => {
                    println!("MISMATCH after applying. Windows did not store what we asked for.");
                    println!("  expected : {}", drift.expected);
                    println!("  actual   : {}", drift.actual);
                }
                Err(err) => println!("could not verify: {}", err),
            }
        }

        "verify" => {
            if !dir.exists() {
                println!("The store does not exist, so there is nothing to verify.");
                return;
            }
            match security::verify(&dir, &desired) {
                Ok(drift) => {
                    if drift.matches {
                        println!("MATCHES the policy.");
                        println!("  {}", drift.actual);
                    } else {
                        println!("DRIFT: what is on disk is not what the policy asks for.");
                        println!("  expected : {}", drift.expected);
                        println!("  actual   : {}", drift.actual);
                        println!();
                        println!("`gdeck security apply` puts it back.");
                    }
                    println!();
                    report_exposure(&drift.actual);
                }
                Err(err) => println!("could not verify: {}", err),
            }
        }

        other => {
            println!("unknown security action: {}", other);
            println!();
            usage();
        }
    }
}

fn cmd_list() {
    let collections = device::enumerate();

    if collections.is_empty() {
        println!("Corsair K95 RGB (1b1c:1b11) not found. Is it plugged in?");
        return;
    }

    let product = collections
        .iter()
        .map(|c| c.product.as_str())
        .find(|p| !p.is_empty())
        .unwrap_or("unknown");

    println!("{}, {} HID collections", product, collections.len());
    println!();

    for c in &collections {
        println!(
            "  usage page 0x{:04X}  usage 0x{:04X}   input {}  output {}  feature {}",
            c.usage_page, c.usage, c.input_len, c.output_len, c.feature_len
        );
        if c.is_g_key_pipe() {
            println!("    >> G key pipe. The only collection this app ever opens.");
        } else if c.usage_page == 0x0001 && c.usage == 0x0006 {
            println!("    >> keyboard. Windows claims this exclusively; we are refused it.");
        } else if c.output_len > 1 {
            println!("    >> writable command channel. We never open this.");
        }
        println!("    {}", c.path);
    }
}

fn cmd_watch(seconds: Option<u64>) {
    let (mut reader, events) = match Reader::start() {
        Ok(pair) => pair,
        Err(err) => {
            eprintln!("could not start the reader: {}", err);
            return;
        }
    };

    match seconds {
        Some(s) => println!("Watching for {} seconds once you begin, then stopping cleanly.", s),
        None => println!("Watching. Press Ctrl+C to stop."),
    }
    println!("Press some G keys. Normal typing is invisible to this process.");

    // A timed window must not start counting down while the operator is still
    // reading. Without this gate, every run races the person using it.
    if seconds.is_some() {
        println!();
        print!("Press Enter to start the window. ");
        let _ = io::stdout().flush();
        let mut ready = String::new();
        let _ = io::stdin().read_line(&mut ready);
    }
    println!();

    let started = Instant::now();
    let deadline = seconds.map(|s| started + Duration::from_secs(s));

    let mut presses = [0u32; G_KEY_COUNT];
    let mut total_presses = 0usize;

    loop {
        let event = match deadline {
            Some(end) => {
                let now = Instant::now();
                if now >= end {
                    break;
                }
                match events.recv_timeout(end - now) {
                    Ok(event) => event,
                    Err(RecvTimeoutError::Timeout) => break,
                    Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            None => match events.recv() {
                Ok(event) => event,
                Err(_) => break,
            },
        };

        let at = started.elapsed().as_secs_f64();
        match event {
            Event::Connected => println!("[{:>7.3}s] keyboard connected", at),
            Event::Disconnected => println!("[{:>7.3}s] keyboard disconnected", at),
            Event::Pressed(key) => {
                presses[key.index()] += 1;
                total_presses += 1;
                println!("[{:>7.3}s] {:<4} down", at, key.to_string());
            }
            Event::Released(key) => {
                println!("[{:>7.3}s] {:<4} up", at, key.to_string());
            }
        }
    }

    // Explicit rather than relying on Drop, so that a clean stop is what is
    // being demonstrated here.
    reader.stop();

    println!();
    println!("Stopped cleanly after {:.1}s.", started.elapsed().as_secs_f64());
    println!();

    if total_presses == 0 {
        println!("No G key presses seen.");
        return;
    }

    println!("{} presses:", total_presses);
    for key in GKey::all() {
        let count = presses[key.index()];
        if count > 0 {
            println!("  {:<4} {}", key.to_string(), count);
        }
    }

    let missing: Vec<String> = GKey::all()
        .filter(|key| presses[key.index()] == 0)
        .map(|key| key.to_string())
        .collect();

    println!();
    if missing.is_empty() {
        println!("All 18 keys registered.");
    } else {
        println!("Not pressed: {}", missing.join(", "));
    }
}
