//! Access control for the config store.
//!
//! The config file is a list of things to execute, so anyone who can write it
//! can run code as you. The store is therefore treated as a security boundary:
//! inheritance severed, no group ACEs, owner pinned to the current user and
//! re-asserted on every start.
//!
//! Everything here is expressed as SDDL, including the built in defaults. That
//! is deliberate: the "paste your own descriptor" control and the shipped
//! policy go through exactly the same code path, so there is no second,
//! less tested route for a custom descriptor to take.
//!
//! What this does **not** do, stated plainly because it would be easy to
//! imply otherwise: it does not stop an administrator. An administrator can
//! take ownership of any object and rewrite its DACL. What it does stop is
//! other standard accounts on the machine, inherited permission accidents, and
//! silent same privilege access, because anyone who is not the owner has to
//! take ownership first, which changes the owner field and is therefore
//! detectable by [`verify`].

pub mod ffi;

use core::ffi::c_void;
use core::ptr;
use std::path::Path;

use crate::ffi::wide;
use crate::{Error, Result};

use ffi::*;

/// A security identifier, held in its string form, which is all we need.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Sid(String);

impl Sid {
    /// The SID of the account this process is running as.
    pub fn current_user() -> Result<Sid> {
        unsafe {
            let mut token: isize = 0;
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return Err(Error::Win32 {
                    what: "OpenProcessToken",
                    code: GetLastError(),
                });
            }

            // Ask for the size first; TOKEN_USER is variable length because the
            // SID is appended after the struct.
            let mut needed: u32 = 0;
            GetTokenInformation(token, TOKEN_USER_CLASS, ptr::null_mut(), 0, &mut needed);
            if needed == 0 {
                let code = GetLastError();
                CloseHandle(token);
                return Err(Error::Win32 {
                    what: "GetTokenInformation(size)",
                    code,
                });
            }

            let mut buf = vec![0u8; needed as usize];
            let ok = GetTokenInformation(
                token,
                TOKEN_USER_CLASS,
                buf.as_mut_ptr() as *mut c_void,
                needed,
                &mut needed,
            );
            let code = GetLastError();
            CloseHandle(token);

            if ok == 0 {
                return Err(Error::Win32 {
                    what: "GetTokenInformation",
                    code,
                });
            }

            let user = &*(buf.as_ptr() as *const TokenUser);
            sid_to_string(user.sid)
        }
    }

    /// Parse from the textual form, e.g. `S-1-5-18`.
    pub fn parse(text: &str) -> Result<Sid> {
        unsafe {
            let mut raw: *mut c_void = ptr::null_mut();
            if ConvertStringSidToSidW(wide(text).as_ptr(), &mut raw) == 0 {
                return Err(Error::Win32 {
                    what: "ConvertStringSidToSidW",
                    code: GetLastError(),
                });
            }
            LocalFree(raw);
        }
        Ok(Sid(text.to_string()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `DOMAIN\Name` for this SID, when it resolves.
    pub fn display_name(&self) -> Option<String> {
        resolve_sid_name(&self.0)
    }
}

/// Render a binary SID as text.
unsafe fn sid_to_string(sid: *mut c_void) -> Result<Sid> {
    if sid.is_null() || IsValidSid(sid) == 0 {
        return Err(Error::Win32 {
            what: "IsValidSid",
            code: 0,
        });
    }
    let mut out: *mut u16 = ptr::null_mut();
    if ConvertSidToStringSidW(sid, &mut out) == 0 {
        return Err(Error::Win32 {
            what: "ConvertSidToStringSidW",
            code: GetLastError(),
        });
    }
    let text = string_from_wide(out);
    LocalFree(out as *mut c_void);
    Ok(Sid(text))
}

/// Best effort `DOMAIN\Name` lookup. Returns `None` for unresolvable SIDs.
pub fn resolve_sid_name(sid_text: &str) -> Option<String> {
    unsafe {
        let mut raw: *mut c_void = ptr::null_mut();
        if ConvertStringSidToSidW(wide(sid_text).as_ptr(), &mut raw) == 0 {
            return None;
        }

        let mut name = vec![0u16; 256];
        let mut name_len = name.len() as u32;
        let mut domain = vec![0u16; 256];
        let mut domain_len = domain.len() as u32;
        let mut kind: u32 = 0;

        let ok = LookupAccountSidW(
            ptr::null(),
            raw,
            name.as_mut_ptr(),
            &mut name_len,
            domain.as_mut_ptr(),
            &mut domain_len,
            &mut kind,
        );
        LocalFree(raw);

        if ok == 0 {
            return None;
        }

        let name = String::from_utf16_lossy(&name[..name_len as usize]);
        let domain = String::from_utf16_lossy(&domain[..domain_len as usize]);
        if domain.is_empty() {
            Some(name)
        } else {
            Some(format!("{}\\{}", domain, name))
        }
    }
}

/// Which trustees the shipped policy grants.
#[derive(Clone, Debug)]
pub struct Policy {
    pub owner: Sid,
    /// Grant `BUILTIN\Administrators` full control.
    ///
    /// Off by default. It does not lock administrators out either way, since
    /// they can take ownership, but leaving it out means they must do so, and
    /// taking ownership is visible to [`verify`].
    pub include_administrators: bool,
}

impl Policy {
    /// Owner and SYSTEM only. The shipped default.
    pub fn lockdown(owner: Sid) -> Policy {
        Policy {
            owner,
            include_administrators: false,
        }
    }

    /// Owner, SYSTEM and `BUILTIN\Administrators`.
    pub fn with_administrators(owner: Sid) -> Policy {
        Policy {
            owner,
            include_administrators: true,
        }
    }

    /// Descriptor for the store directory. `OICI` so new files inherit it.
    ///
    /// Owner and DACL only. There is deliberately no `G:` component: the
    /// primary group on a Windows file object is a vestigial POSIX
    /// compatibility field that nothing here reads, and claiming a value we do
    /// not actually write would make [`verify`] report drift forever.
    pub fn directory_sddl(&self) -> String {
        let sid = self.owner.as_str();
        let mut s = format!("O:{sid}D:PAI(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)", sid = sid);
        if self.include_administrators {
            s.push_str("(A;OICI;FA;;;BA)");
        }
        s
    }

    /// Descriptor for the audit log: read and append, but deliberately not
    /// `FILE_WRITE_DATA`, so entries already written cannot be overwritten in
    /// place.
    ///
    /// `0x13019D` is `FILE_GENERIC_READ` plus `FILE_APPEND_DATA` (`0x004`),
    /// `FILE_WRITE_EA` (`0x010`), `FILE_WRITE_ATTRIBUTES` (`0x100`) and
    /// `DELETE` (`0x10000`).
    ///
    /// The write EA and write attributes bits are not decoration. Opening a
    /// file in append mode requests them alongside `FILE_APPEND_DATA`, so a
    /// mask of exactly read-plus-append is refused at open time and the log
    /// silently stops recording. Neither grants any ability to alter existing
    /// content.
    ///
    /// `DELETE` is present so the log can be rotated and so you can clear your
    /// own history. Withholding it bought no real security: the owner holds
    /// `WRITE_DAC` implicitly and can rewrite this descriptor regardless, and
    /// a deleted log is conspicuous whereas a quietly edited entry is not.
    /// Without it the file was undeletable and unrotatable, which is a worse
    /// problem than the one it solved.
    ///
    /// `FILE_WRITE_DATA` (`0x002`) remains withheld. That is the bit that would
    /// allow rewriting what is already there, and it is the protection with
    /// actual value here.
    pub fn audit_sddl(&self) -> String {
        let sid = self.owner.as_str();
        let mut s = format!("O:{sid}D:PAI(A;;0x13019D;;;{sid})(A;;FA;;;SY)", sid = sid);
        if self.include_administrators {
            s.push_str("(A;;FA;;;BA)");
        }
        s
    }

    /// Descriptor for a leaf file. The owner gets read, write and delete
    /// rather than full control, so changing the descriptor itself is a
    /// deliberate act rather than something a stray handle can do.
    pub fn file_sddl(&self) -> String {
        let sid = self.owner.as_str();
        let mut s = format!("O:{sid}D:PAI(A;;0x1301BF;;;{sid})(A;;FA;;;SY)", sid = sid);
        if self.include_administrators {
            s.push_str("(A;;FA;;;BA)");
        }
        s
    }
}

/// The owner component of a descriptor, e.g. `LA` or `S-1-5-21-...`.
///
/// Only meaningful on a string that came out of [`normalize_sddl`] or
/// [`read_sddl`], so that both sides of a comparison were spelled by Win32.
pub fn owner_of(sddl: &str) -> Option<String> {
    let start = sddl.find("O:")? + 2;
    let rest = &sddl[start..];
    let end = ["G:", "D:", "S:"]
        .iter()
        .filter_map(|marker| rest.find(marker))
        .min()
        .unwrap_or(rest.len());
    Some(rest[..end].to_string())
}

/// Apply a descriptor to a path, severing inheritance.
///
/// `PROTECTED_DACL_SECURITY_INFORMATION` is what makes the DACL protected: no
/// ACE from a parent directory can flow in, now or later.
///
/// The owner is only written when it actually differs. Setting an owner
/// requires `WRITE_OWNER`, which an owner does **not** hold implicitly (only
/// `READ_CONTROL` and `WRITE_DAC` are implicit), and the file policy
/// deliberately withholds it. Asking unconditionally made this call fail with
/// ACCESS_DENIED the second time it ran against any file, which is to say it
/// was not idempotent.
pub fn apply(path: &Path, sddl: &str) -> Result<()> {
    let text = path.to_string_lossy().to_string();

    let desired_owner = normalize_sddl(sddl).ok().and_then(|s| owner_of(&s));
    let current_owner = read_sddl(path).ok().and_then(|s| owner_of(&s));
    let owner_needs_setting = match (&desired_owner, &current_owner) {
        (Some(desired), Some(current)) => desired != current,
        (Some(_), None) => true,
        _ => false,
    };

    unsafe {
        let mut descriptor: *mut c_void = ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide(sddl).as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        ) == 0
        {
            return Err(Error::Win32 {
                what: "ConvertStringSecurityDescriptorToSecurityDescriptorW",
                code: GetLastError(),
            });
        }

        let mut owner: *mut c_void = ptr::null_mut();
        let mut defaulted: i32 = 0;
        GetSecurityDescriptorOwner(descriptor, &mut owner, &mut defaulted);

        let mut dacl: *mut c_void = ptr::null_mut();
        let mut present: i32 = 0;
        let mut dacl_defaulted: i32 = 0;
        GetSecurityDescriptorDacl(descriptor, &mut present, &mut dacl, &mut dacl_defaulted);

        let mut information = DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION;
        if owner_needs_setting {
            information |= OWNER_SECURITY_INFORMATION;
        }

        let mut name = wide(&text);
        let status = SetNamedSecurityInfoW(
            name.as_mut_ptr(),
            SE_FILE_OBJECT,
            information,
            if owner_needs_setting {
                owner
            } else {
                ptr::null_mut()
            },
            ptr::null_mut(),
            if present != 0 { dacl } else { ptr::null_mut() },
            ptr::null_mut(),
        );

        LocalFree(descriptor);

        if status != ERROR_SUCCESS {
            return Err(Error::Win32 {
                what: "SetNamedSecurityInfoW",
                code: status,
            });
        }
    }

    Ok(())
}

/// The descriptor currently on a path, as SDDL.
pub fn read_sddl(path: &Path) -> Result<String> {
    let text = path.to_string_lossy().to_string();

    unsafe {
        let mut descriptor: *mut c_void = ptr::null_mut();
        let mut owner: *mut c_void = ptr::null_mut();
        let mut group: *mut c_void = ptr::null_mut();
        let mut dacl: *mut c_void = ptr::null_mut();

        // Owner and DACL only, matching what `apply` actually writes. Asking
        // for the group here would compare a field we never set.
        let status = GetNamedSecurityInfoW(
            wide(&text).as_ptr(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            &mut group,
            &mut dacl,
            ptr::null_mut(),
            &mut descriptor,
        );
        if status != ERROR_SUCCESS {
            return Err(Error::Win32 {
                what: "GetNamedSecurityInfoW",
                code: status,
            });
        }

        let mut out: *mut u16 = ptr::null_mut();
        let mut out_len: u32 = 0;
        let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut out,
            &mut out_len,
        );
        let code = GetLastError();

        if ok == 0 {
            LocalFree(descriptor);
            return Err(Error::Win32 {
                what: "ConvertSecurityDescriptorToStringSecurityDescriptorW",
                code,
            });
        }

        let sddl = string_from_wide(out);
        LocalFree(out as *mut c_void);
        LocalFree(descriptor);
        Ok(sddl)
    }
}

/// Round trip a descriptor through Win32 so two descriptors can be compared
/// without being defeated by spelling differences such as `FA` versus
/// `0x1F01FF`.
pub fn normalize_sddl(sddl: &str) -> Result<String> {
    unsafe {
        let mut descriptor: *mut c_void = ptr::null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            wide(sddl).as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        ) == 0
        {
            return Err(Error::Win32 {
                what: "ConvertStringSecurityDescriptorToSecurityDescriptorW",
                code: GetLastError(),
            });
        }

        let mut out: *mut u16 = ptr::null_mut();
        let mut out_len: u32 = 0;
        let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor,
            SDDL_REVISION_1,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut out,
            &mut out_len,
        );
        let code = GetLastError();

        if ok == 0 {
            LocalFree(descriptor);
            return Err(Error::Win32 {
                what: "ConvertSecurityDescriptorToStringSecurityDescriptorW",
                code,
            });
        }

        let text = string_from_wide(out);
        LocalFree(out as *mut c_void);
        LocalFree(descriptor);
        Ok(text)
    }
}

/// Result of comparing what is on disk against what the policy wants.
#[derive(Clone, Debug)]
pub struct Drift {
    pub path: String,
    pub matches: bool,
    pub expected: String,
    pub actual: String,
}

/// Compare the descriptor on `path` against `desired`.
pub fn verify(path: &Path, desired: &str) -> Result<Drift> {
    let actual = normalize_sddl(&read_sddl(path)?)?;
    let expected = normalize_sddl(desired)?;
    Ok(Drift {
        path: path.to_string_lossy().to_string(),
        matches: actual == expected,
        expected,
        actual,
    })
}

/// One access control entry, for display.
///
/// This parses SDDL only to show it to a person. No security decision is made
/// from the result; comparisons go through [`normalize_sddl`] and Win32.
#[derive(Clone, Debug)]
pub struct Ace {
    pub raw: String,
    pub kind: String,
    pub flags: String,
    pub rights: String,
    pub trustee: String,
    pub trustee_name: Option<String>,
}

/// Pull the ACEs out of the `D:` section of a descriptor.
pub fn parse_aces(sddl: &str) -> Vec<Ace> {
    let dacl = match sddl.find("D:") {
        Some(at) => &sddl[at + 2..],
        None => return Vec::new(),
    };
    // Stop at a SACL if one is present.
    let dacl = match dacl.find("S:") {
        Some(at) => &dacl[..at],
        None => dacl,
    };

    let mut aces = Vec::new();
    let mut rest = dacl;

    while let Some(open) = rest.find('(') {
        let after = &rest[open + 1..];
        let close = match after.find(')') {
            Some(c) => c,
            None => break,
        };
        let body = &after[..close];
        rest = &after[close + 1..];

        let fields: Vec<&str> = body.split(';').collect();
        if fields.len() < 6 {
            continue;
        }
        let trustee = fields[5].to_string();
        aces.push(Ace {
            raw: format!("({})", body),
            kind: describe_ace_type(fields[0]),
            flags: describe_flags(fields[1]),
            rights: describe_rights(fields[2]),
            trustee_name: describe_trustee(&trustee),
            trustee,
        });
    }

    aces
}

fn describe_ace_type(code: &str) -> String {
    match code {
        "A" => "ALLOW".to_string(),
        "D" => "DENY".to_string(),
        other => other.to_string(),
    }
}

fn describe_flags(flags: &str) -> String {
    if flags.is_empty() {
        return "none (leaf)".to_string();
    }
    let mut parts = Vec::new();
    let upper = flags.to_uppercase();
    if upper.contains("OI") {
        parts.push("object inherit");
    }
    if upper.contains("CI") {
        parts.push("container inherit");
    }
    if upper.contains("NP") {
        parts.push("no propagate");
    }
    if upper.contains("IO") {
        parts.push("inherit only");
    }
    if upper.contains("ID") {
        parts.push("inherited");
    }
    if parts.is_empty() {
        flags.to_string()
    } else {
        parts.join(", ")
    }
}

fn describe_rights(rights: &str) -> String {
    match rights.to_uppercase().as_str() {
        "FA" => "full control (0x1F01FF)".to_string(),
        "FR" => "read (0x120089)".to_string(),
        "FW" => "write (0x120116)".to_string(),
        "FX" => "execute (0x1200A0)".to_string(),
        "0X1301BF" => "read, write, delete (0x1301BF)".to_string(),
        "0X13019D" => "read, append and delete, no overwrite (0x13019D)".to_string(),
        other => other.to_string(),
    }
}

fn describe_trustee(trustee: &str) -> Option<String> {
    match trustee.to_uppercase().as_str() {
        "SY" => Some("NT AUTHORITY\\SYSTEM".to_string()),
        "BA" => Some("BUILTIN\\Administrators".to_string()),
        "BU" => Some("BUILTIN\\Users".to_string()),
        "WD" => Some("Everyone".to_string()),
        "AU" => Some("Authenticated Users".to_string()),
        "CO" => Some("CREATOR OWNER".to_string()),
        "AC" => Some("ALL APPLICATION PACKAGES".to_string()),
        _ => resolve_sid_name(trustee),
    }
}

/// Trustees that must never appear on the store, and why.
pub const FORBIDDEN_TRUSTEES: [(&str, &str, &str); 5] = [
    ("WD", "S-1-1-0", "Everyone: any process on the box could rewrite what you launch"),
    ("BU", "S-1-5-32-545", "BUILTIN\\Users: lets a second local account tamper with your config"),
    ("AU", "S-1-5-11", "Authenticated Users: same exposure, wider"),
    ("CO", "S-1-3-0", "CREATOR OWNER: grants by creation rather than identity"),
    ("AC", "S-1-15-2-1", "ALL APPLICATION PACKAGES: exposes the store to sandboxed Store apps"),
];

/// Any forbidden trustee present in a descriptor.
pub fn forbidden_present(sddl: &str) -> Vec<&'static str> {
    let aces = parse_aces(sddl);
    let mut found = Vec::new();
    for (alias, sid, why) in FORBIDDEN_TRUSTEES.iter() {
        let hit = aces.iter().any(|ace| {
            ace.kind == "ALLOW"
                && (ace.trustee.eq_ignore_ascii_case(alias) || ace.trustee.eq_ignore_ascii_case(sid))
        });
        if hit {
            found.push(*why);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid() -> Sid {
        Sid("S-1-5-21-1-2-3-1000".to_string())
    }

    #[test]
    fn lockdown_grants_only_owner_and_system() {
        let sddl = Policy::lockdown(sid()).directory_sddl();
        assert!(sddl.contains("D:PAI"), "DACL must be protected");
        assert!(sddl.contains("(A;OICI;FA;;;S-1-5-21-1-2-3-1000)"));
        assert!(sddl.contains("(A;OICI;FA;;;SY)"));
        assert!(!sddl.contains(";BA)"), "administrators must be absent by default");
    }

    #[test]
    fn policy_claims_no_primary_group() {
        // We do not write the group, so the descriptor must not assert one.
        // Claiming it made verify report permanent false drift.
        for policy in [Policy::lockdown(sid()), Policy::with_administrators(sid())] {
            assert!(!policy.directory_sddl().contains("G:"));
            assert!(!policy.file_sddl().contains("G:"));
        }
    }

    #[test]
    fn administrators_is_opt_in() {
        let sddl = Policy::with_administrators(sid()).directory_sddl();
        assert!(sddl.contains("(A;OICI;FA;;;BA)"));
    }

    #[test]
    fn file_policy_is_protected_and_not_full_control_for_owner() {
        let sddl = Policy::lockdown(sid()).file_sddl();
        assert!(sddl.contains("D:PAI"));
        assert!(sddl.contains("0x1301BF"));
    }

    #[test]
    fn no_default_policy_contains_a_forbidden_trustee() {
        for policy in [Policy::lockdown(sid()), Policy::with_administrators(sid())] {
            assert!(forbidden_present(&policy.directory_sddl()).is_empty());
            assert!(forbidden_present(&policy.file_sddl()).is_empty());
        }
    }

    #[test]
    fn forbidden_trustees_are_detected() {
        let bad = "O:S-1-5-21-1-2-3-1000D:PAI(A;OICI;FA;;;WD)(A;OICI;FA;;;SY)";
        assert_eq!(forbidden_present(bad).len(), 1);

        let worse = "O:S-1-5-21-1-2-3-1000D:AI(A;OICI;FA;;;BU)(A;OICI;FA;;;AU)";
        assert_eq!(forbidden_present(worse).len(), 2);
    }

    #[test]
    fn aces_parse_into_readable_parts() {
        let aces = parse_aces(&Policy::lockdown(sid()).directory_sddl());
        assert_eq!(aces.len(), 2);
        assert_eq!(aces[0].kind, "ALLOW");
        assert_eq!(aces[0].rights, "full control (0x1F01FF)");
        assert_eq!(aces[0].flags, "object inherit, container inherit");
        assert_eq!(aces[1].trustee_name.as_deref(), Some("NT AUTHORITY\\SYSTEM"));
    }

    /// The access mask from the first ACE of a descriptor, as a number.
    fn first_ace_mask(sddl: &str) -> u32 {
        let aces = parse_aces(sddl);
        let raw = aces[0].raw.trim_matches(|c| c == '(' || c == ')').to_string();
        let rights = raw.split(';').nth(2).expect("rights field");
        let hex = rights.trim_start_matches("0x").trim_start_matches("0X");
        u32::from_str_radix(hex, 16).expect("hex mask")
    }

    #[test]
    fn the_audit_mask_can_append_but_cannot_overwrite() {
        // Regression test. A mask of exactly read-plus-append looks right and
        // is wrong: opening a file in append mode also requests
        // FILE_WRITE_ATTRIBUTES and FILE_WRITE_EA, so without them every
        // append is refused and the log silently stops recording.
        const FILE_WRITE_DATA: u32 = 0x0002;
        const FILE_APPEND_DATA: u32 = 0x0004;
        const FILE_WRITE_EA: u32 = 0x0010;
        const FILE_READ_DATA: u32 = 0x0001;
        const FILE_WRITE_ATTRIBUTES: u32 = 0x0100;
        const DELETE: u32 = 0x0001_0000;

        let mask = first_ace_mask(&Policy::lockdown(sid()).audit_sddl());

        assert_eq!(
            mask & FILE_WRITE_DATA,
            0,
            "FILE_WRITE_DATA would allow rewriting existing entries"
        );
        assert_ne!(mask & FILE_APPEND_DATA, 0, "cannot append");
        assert_ne!(mask & FILE_WRITE_ATTRIBUTES, 0, "append mode opens need this");
        assert_ne!(mask & FILE_WRITE_EA, 0, "append mode opens need this");
        assert_ne!(mask & FILE_READ_DATA, 0, "the log has to be readable");
        assert_ne!(
            mask & DELETE,
            0,
            "without DELETE the log cannot be rotated or cleared, ever"
        );
    }

    #[test]
    fn the_config_mask_can_write_but_cannot_take_ownership() {
        // WRITE_OWNER would let a holder of the file re-own it. The owner can
        // still change the descriptor, because WRITE_DAC is implicit for an
        // owner, which is why `apply` only sets the owner when it differs.
        const WRITE_OWNER: u32 = 0x0008_0000;
        const FILE_WRITE_DATA: u32 = 0x0002;

        let mask = first_ace_mask(&Policy::lockdown(sid()).file_sddl());
        assert_ne!(mask & FILE_WRITE_DATA, 0);
        assert_eq!(mask & WRITE_OWNER, 0);
    }

    #[test]
    fn the_owner_component_is_extracted_in_each_spelling() {
        assert_eq!(owner_of("O:LAD:PAI(A;;FA;;;SY)").as_deref(), Some("LA"));
        assert_eq!(
            owner_of("O:S-1-5-21-1-2-3-1000D:PAI(A;;FA;;;SY)").as_deref(),
            Some("S-1-5-21-1-2-3-1000")
        );
        // Still correct when a group component is present, which descriptors
        // read back from disk may well have even though we never write one.
        assert_eq!(owner_of("O:LAG:DUD:PAI(A;;FA;;;SY)").as_deref(), Some("LA"));
        assert_eq!(owner_of("D:PAI(A;;FA;;;SY)"), None);
    }

    #[test]
    fn a_deny_ace_is_not_read_as_a_grant() {
        // A DENY ace for Everyone is not an exposure, and must not be reported
        // as one.
        let denied = "O:S-1-5-21-1-2-3-1000D:PAI(D;OICI;FA;;;WD)(A;OICI;FA;;;SY)";
        assert!(forbidden_present(denied).is_empty());
    }
}
