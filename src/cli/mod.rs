//! The command line surface.
//!
//! Every command here works without the network. They read and write the local
//! store only. Presence needs the running application, so `list` reports the
//! last time a contact was reachable, not whether they are online now.

pub mod doctor;
mod fmt;
pub mod tui;
pub mod update;

use crate::error::{Error, Result};
use crate::store::{Store, identity, ticket::Ticket};

use fmt::{Table, box_line, relative_time};

/// `swivel key`
pub fn key(copy: bool) -> Result<()> {
    let store = Store::open()?;
    let me = store.identity(&identity::default_name())?;
    let ticket = Ticket::new(me.endpoint_id(), &me.name).encode();

    if copy {
        copy_to_clipboard(&ticket)?;
        println!("Your key is on the clipboard. Send it to a friend.");
        return Ok(());
    }

    println!();
    println!("{}", box_line::top("YOUR KEY", 68));
    println!("  {ticket}");
    println!("{}", box_line::bottom(68));
    println!();
    println!("  Send this to a friend. They run:");
    // Never print a shortened key here. A reader will copy whatever looks like
    // a key, and a shortened one fails with a confusing message.
    println!("      swivel add <the key above>");
    println!();
    println!("  It carries your public key and the name {:?}.", me.name);
    println!("  It carries no secret.");
    println!();
    Ok(())
}

/// `swivel add <ticket>`
pub fn add(ticket_text: &str, name_override: Option<&str>) -> Result<()> {
    let store = Store::open()?;
    let me = store.identity(&identity::default_name())?;
    let ticket = Ticket::decode(ticket_text)?;

    if ticket.endpoint_id == me.endpoint_id() {
        return Err(Error::Ticket("that is your own key".into()));
    }

    let name = name_override.unwrap_or(&ticket.name);
    let existed = store.contact(ticket.endpoint_id)?.is_some();
    let contact = store.add_contact(ticket.endpoint_id, name)?;

    let verb = if existed { "updated" } else { "added" };
    match contact.slot {
        Some(slot) => {
            println!();
            println!("  {verb} {} as slot {slot}", contact.name);
            println!();
            println!("  Press  ⌃⌥⌘T  then  {slot}  to talk to them.");
            println!();
        }
        None => {
            println!();
            println!("  {verb} {}", contact.name);
            println!();
            println!("  Slots 1 to 9 are taken, so this contact has no number.");
            println!("  Use `swivel slot {} <n>` to give them one.", contact.name);
            println!();
        }
    }

    if !existed {
        println!("  They must approve you before audio flows. Send them your key:");
        println!("      swivel key");
        println!();
    }
    Ok(())
}

/// `swivel list`
pub fn list() -> Result<()> {
    let store = Store::open()?;
    let contacts = store.contacts()?;
    let knocks = store.pending_knocks()?;

    if contacts.is_empty() && knocks.is_empty() {
        println!();
        println!("  No contacts yet.");
        println!();
        println!("  Send your key to a friend:   swivel key");
        println!("  Add theirs:                  swivel add sv1…");
        println!();
        return Ok(());
    }

    if !contacts.is_empty() {
        let mut table = Table::new(["SLOT", "NAME", "LAST SEEN", "OPENS", "KEY"]);
        for c in &contacts {
            table.row([
                c.slot.map(|s| s.to_string()).unwrap_or_else(|| "-".into()),
                c.name.clone(),
                c.last_seen
                    .map(relative_time)
                    .unwrap_or_else(|| "never".into()),
                if c.auto_open {
                    "auto".into()
                } else {
                    "knock".into()
                },
                c.endpoint_id.fmt_short().to_string(),
            ]);
        }
        println!();
        println!("{}", box_line::label("CONTACTS"));
        println!();
        table.print("  ");
        println!();
    }

    if !knocks.is_empty() {
        println!("{}", box_line::label("WAITING FOR YOUR APPROVAL"));
        println!();
        let mut table = Table::new(["NAME", "FIRST SEEN", "KEY"]);
        for k in &knocks {
            table.row([
                k.claimed.clone().unwrap_or_else(|| "(no name)".into()),
                relative_time(k.first_seen),
                k.endpoint_id.fmt_short().to_string(),
            ]);
        }
        table.print("  ");
        println!();
        println!("  Approve one with:  swivel approve <key>");
        println!();
    }

    Ok(())
}

/// `swivel rm <who>`
pub fn remove(who: &str) -> Result<()> {
    let store = Store::open()?;
    let contact = store.find_contact(who)?;
    store.remove_contact(contact.endpoint_id)?;

    println!();
    println!("  removed {}", contact.name);
    if let Some(slot) = contact.slot {
        println!("  slot {slot} is free again");
    }
    println!();
    Ok(())
}

/// `swivel slot <who> <n>`
pub fn set_slot(who: &str, slot: u8) -> Result<()> {
    let store = Store::open()?;
    let contact = store.find_contact(who)?;
    let displaced = store.contact_by_slot(slot)?;

    store.set_slot(contact.endpoint_id, slot)?;

    println!();
    println!("  {} is now slot {slot}", contact.name);
    if let Some(d) = displaced
        && d.endpoint_id != contact.endpoint_id
    {
        let moved = store.contact(d.endpoint_id)?.and_then(|c| c.slot);
        match moved {
            Some(s) => println!("  {} moved to slot {s}", d.name),
            None => println!("  {} has no slot now", d.name),
        }
    }
    println!();
    Ok(())
}

/// `swivel devices`
pub fn devices(input: Option<&str>, output: Option<&str>, reset: bool) -> Result<()> {
    use crate::audio::device::{self, Direction};
    use crate::store::{SETTING_INPUT_DEVICE, SETTING_OUTPUT_DEVICE};

    let store = Store::open()?;

    if reset {
        store.set_setting(SETTING_INPUT_DEVICE, None)?;
        store.set_setting(SETTING_OUTPUT_DEVICE, None)?;
        println!("\n  both devices follow the system default again\n");
        return Ok(());
    }

    let mut changed = false;

    for (wanted, direction, key) in [
        (input, Direction::Input, SETTING_INPUT_DEVICE),
        (output, Direction::Output, SETTING_OUTPUT_DEVICE),
    ] {
        let Some(wanted) = wanted else { continue };

        let available = device::names(direction);
        let chosen = resolve_device(wanted, &available)?;

        store.set_setting(key, Some(&chosen))?;
        println!("\n  {direction:?} device set to {chosen}");
        changed = true;
    }

    if changed {
        println!("\n  Restart swivel, or it applies on the next conversation.\n");
        return Ok(());
    }

    // No change asked for, so list what there is.
    let current_in = store.setting(SETTING_INPUT_DEVICE)?;
    let current_out = store.setting(SETTING_OUTPUT_DEVICE)?;

    for (direction, current) in [
        (Direction::Input, &current_in),
        (Direction::Output, &current_out),
    ] {
        println!();
        println!(
            "{}",
            box_line::label(&format!("{direction:?}").to_uppercase())
        );
        println!();

        let names = device::names(direction);
        if names.is_empty() {
            println!("    none");
            continue;
        }

        let default = device::default_name(direction);
        let mut table = Table::new(["", "N", "DEVICE", ""]);

        for (index, name) in names.iter().enumerate() {
            let in_use = match current {
                Some(chosen) => chosen == name,
                None => default.as_deref() == Some(name.as_str()),
            };
            table.row([
                if in_use { "*".into() } else { " ".into() },
                (index + 1).to_string(),
                name.clone(),
                if current.is_none() && default.as_deref() == Some(name.as_str()) {
                    "system default".into()
                } else {
                    String::new()
                },
            ]);
        }
        table.print("  ");
    }

    println!();
    println!("  Choose one by number or by name:");
    println!("      swivel devices --in 2 --out \"External Headphones\"");
    println!("      swivel devices --reset");
    println!();
    Ok(())
}

/// Turns a number or a name fragment into a device name.
fn resolve_device(wanted: &str, available: &[String]) -> Result<String> {
    if available.is_empty() {
        return Err(Error::Audio("this machine reports no such devices".into()));
    }

    if let Ok(number) = wanted.parse::<usize>()
        && number >= 1
        && number <= available.len()
    {
        return Ok(available[number - 1].clone());
    }

    let lower = wanted.to_lowercase();
    let matches: Vec<&String> = available
        .iter()
        .filter(|name| name.to_lowercase().contains(&lower))
        .collect();

    match matches.len() {
        1 => Ok(matches[0].clone()),
        0 => Err(Error::Audio(format!(
            "no device matches {wanted:?}. Run `swivel devices` to list them."
        ))),
        _ => Err(Error::Audio(format!(
            "{wanted:?} matches {} devices: {}",
            matches.len(),
            matches
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        ))),
    }
}

/// `swivel approve <key>`
pub fn approve(who: &str, name: Option<&str>) -> Result<()> {
    let store = Store::open()?;
    let knocks = store.pending_knocks()?;
    let lower = who.trim().to_lowercase();

    let knock = knocks
        .iter()
        .find(|k| {
            k.endpoint_id.to_string().starts_with(&lower)
                || k.claimed
                    .as_deref()
                    .is_some_and(|c| c.to_lowercase() == lower)
        })
        .ok_or_else(|| Error::NoSuchContact(who.to_string()))?;

    store.approve_knock(knock.endpoint_id, name)?;
    let contact = store
        .contact(knock.endpoint_id)?
        .ok_or_else(|| Error::NoSuchContact(who.to_string()))?;

    println!();
    match contact.slot {
        Some(slot) => println!("  approved {} as slot {slot}", contact.name),
        None => println!("  approved {}. No slot is free.", contact.name),
    }
    println!();
    Ok(())
}

/// `swivel block <key>`
pub fn block(who: &str) -> Result<()> {
    let store = Store::open()?;

    // A block may target a contact or a knock.
    if let Ok(contact) = store.find_contact(who) {
        store.remove_contact(contact.endpoint_id)?;
        store.block(contact.endpoint_id)?;
        println!("\n  blocked {}\n", contact.name);
        return Ok(());
    }

    let lower = who.trim().to_lowercase();
    let knock = store
        .pending_knocks()?
        .into_iter()
        .find(|k| k.endpoint_id.to_string().starts_with(&lower))
        .ok_or_else(|| Error::NoSuchContact(who.to_string()))?;

    store.block(knock.endpoint_id)?;
    println!("\n  blocked {}\n", knock.endpoint_id.fmt_short());
    Ok(())
}

/// Puts text on the macOS clipboard through `pbcopy`.
fn copy_to_clipboard(text: &str) -> Result<()> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|e| Error::Other(anyhow::anyhow!("cannot run pbcopy: {e}")))?;

    child
        .stdin
        .as_mut()
        .ok_or_else(|| Error::Other(anyhow::anyhow!("pbcopy took no input")))?
        .write_all(text.as_bytes())?;

    let status = child.wait()?;
    if !status.success() {
        return Err(Error::Other(anyhow::anyhow!("pbcopy failed")));
    }
    Ok(())
}
