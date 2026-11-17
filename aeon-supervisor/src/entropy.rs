//! Where long-lived keys get their randomness, and whether it can be trusted
//! right now.
//!
//! Everything on the Orb draws from the Linux kernel's random generator, and
//! that is the right default: it is seeded from hardware noise, timing jitter
//! and a saved seed, and `getrandom(2)` will not answer before it is ready.
//! For the two keys that are meant to last — a publisher identity and the
//! Orb's own signing key — this module goes one step further and does not
//! rest on a single source:
//!
//!   * bytes from the kernel generator,
//!   * bytes read DIRECTLY from the board's hardware RNG (`/dev/hwrng`), and
//!   * anything the person chose to add themselves (dice rolls, mashed keys)
//!
//! are hashed together into the secret. A hash of several independent inputs
//! is as unpredictable as the best of them, so each extra source can only
//! help: a weak or missing one costs nothing, and for the key to be guessable
//! every source would have to fail at once.
//!
//! `health()` reports the state of those sources for the console, and
//! `strong_secret()` refuses to make a key at all if the kernel generator has
//! not finished seeding.

use rand::RngCore;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const HWRNG_DEV: &str = "/dev/hwrng";
const HWRNG_SYS: &str = "/sys/class/misc/hw_random";
/// Longest free-text entropy a person may add (it is hashed, so more is not
/// "stronger" past a point; this only bounds the request).
pub const MAX_PERSONAL: usize = 2048;

/// True once the kernel's random pool has been seeded. Asked the precise way:
/// a non-blocking `getrandom` fails with EAGAIN only before that moment. (The
/// `entropy_avail` counter is not used for this — on older kernels it dips
/// during normal use and would cause false alarms.)
pub fn kernel_ready() -> bool {
    #[cfg(target_os = "linux")]
    {
        let mut b = [0u8; 1];
        // The raw syscall, not the libc wrapper: `getrandom()` only exists in
        // glibc 2.25+, and the Orb's binaries are linked against an older one
        // so the same build runs on every supported OS release.
        // SAFETY: `b` is a valid, writable 1-byte buffer for the duration of the call.
        let r = unsafe { libc::syscall(libc::SYS_getrandom, b.as_mut_ptr(), 1usize, libc::GRND_NONBLOCK) };
        r == 1 || std::io::Error::last_os_error().raw_os_error() != Some(libc::EAGAIN)
    }
    #[cfg(not(target_os = "linux"))]
    {
        true
    }
}

/// Read exactly `n` bytes from a character device without ever hanging: the
/// hardware RNG can be absent (ENODEV), slow, or wedged.
fn read_device(path: &str, n: usize, within: Duration) -> Option<Vec<u8>> {
    use std::io::Read;
    use std::os::unix::fs::OpenOptionsExt;
    let mut f = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(path).ok()?;
    let deadline = Instant::now() + within;
    let mut out = vec![0u8; n];
    let mut got = 0;
    while got < n {
        match f.read(&mut out[got..]) {
            Ok(0) => return None,
            Ok(k) => got += k,
            Err(e) if matches!(e.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted) => {
                if Instant::now() >= deadline {
                    return None;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return None,
        }
    }
    Some(out)
}

/// A stuck or dead generator returns the same byte over and over. Thirty-two
/// honest random bytes essentially never contain fewer than eight distinct
/// values, so anything below that is treated as a fault and not used.
fn plausible(bytes: &[u8]) -> bool {
    let mut seen = [false; 256];
    let distinct = bytes.iter().filter(|b| !std::mem::replace(&mut seen[**b as usize], true)).count();
    bytes.len() >= 16 && distinct >= bytes.len() / 4
}

/// Bytes straight from the hardware RNG, or None if the board has none, it
/// does not answer within a second, or what it returns looks stuck.
pub fn hardware_bytes(n: usize) -> Option<Vec<u8>> {
    read_device(HWRNG_DEV, n, Duration::from_secs(1)).filter(|b| plausible(b))
}

/// Combine the sources into one 32-byte secret. Each part is length-prefixed
/// and the whole is domain-separated, so no two different sets of inputs can
/// hash alike by running into each other.
pub fn mix(kernel: &[u8], hardware: Option<&[u8]>, personal: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"aeon-keygen/1");
    for part in [kernel, hardware.unwrap_or(&[]), personal] {
        h.update((part.len() as u64).to_le_bytes());
        h.update(part);
    }
    h.finalize().into()
}

/// A fresh secret for a long-lived key, and which sources went into it.
pub struct Secret {
    pub bytes: [u8; 32],
    pub sources: Vec<&'static str>,
}

/// Make a 32-byte secret for a signing key. `personal` is whatever the person
/// typed to add their own randomness; empty is fine.
pub fn strong_secret(personal: &str) -> Result<Secret, String> {
    if !kernel_ready() {
        return Err("this Orb's random generator has not finished starting up — wait a minute and try again".into());
    }
    let mut kernel = [0u8; 32];
    rand::rngs::OsRng
        .try_fill_bytes(&mut kernel)
        .map_err(|e| format!("the system random generator failed: {e}"))?;
    let hardware = hardware_bytes(32);
    let personal: String = personal.trim().chars().take(MAX_PERSONAL).collect();

    let mut sources = vec!["kernel"];
    if hardware.is_some() {
        sources.push("hardware");
    }
    if !personal.is_empty() {
        sources.push("personal");
    }
    Ok(Secret { bytes: mix(&kernel, hardware.as_deref(), personal.as_bytes()), sources })
}

/// The state of the Orb's randomness, for the console.
#[derive(Debug, Clone, Serialize)]
pub struct Health {
    /// "good" = the system generator is ready and is backed by a hardware
    /// source (read directly here, or feeding the host's kernel);
    /// "ok" = ready, but no hardware source could be found at all;
    /// "not_ready" = the system generator has not finished seeding.
    pub verdict: &'static str,
    pub summary: String,
    pub kernel_ready: bool,
    /// The kernel's own entropy estimate, where it exposes one (informational).
    pub entropy_avail: Option<u32>,
    /// The hardware RNG the kernel is fed by, if any (e.g. "107d208000.rng"
    /// on a Pi 5, "virtio_rng.0" in a VM, "tpm-rng-0" on a PC).
    pub hardware: Option<String>,
    /// True when THIS program can also read that hardware directly and mix it
    /// in as a second, independent source. False inside a container, where the
    /// device is not exposed — the host's kernel still uses it.
    pub hardware_ok: bool,
    /// The processor has its own random instruction (RDRAND/RDSEED, or RNDR).
    pub cpu_rng: bool,
    /// What a signing key made right now would be mixed from.
    pub sources: Vec<&'static str>,
    /// Suggest the person adds their own randomness (scribble / dice): there
    /// is no second source this program can read for itself.
    pub recommend_personal: bool,
}

fn read_trimmed(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok().map(|s| s.trim().to_string()).filter(|s| !s.is_empty())
}

/// Does the processor advertise a hardware random instruction?
fn cpu_has_rng(cpuinfo: &str) -> bool {
    cpuinfo.lines().any(|l| {
        let l = l.to_ascii_lowercase();
        (l.starts_with("flags") || l.starts_with("features"))
            && l.split(|c: char| !c.is_ascii_alphanumeric()).any(|w| matches!(w, "rdrand" | "rdseed" | "rng"))
    })
}

/// The verdict and the sentence shown to the operator. Separated from the
/// probing so every situation — a Pi, a container on a desktop, a board with
/// nothing — can be checked without that hardware.
fn classify(kernel_ready: bool, device_present: bool, direct_ok: bool, kernel_hw: Option<&str>, cpu_rng: bool) -> (&'static str, String) {
    if !kernel_ready {
        return ("not_ready", "The system random generator has not finished starting up. Keys cannot be created yet.".into());
    }
    if direct_ok {
        return ("good", "Keys are mixed from the system generator and this board's hardware random-number chip, read directly.".into());
    }
    let backing = match (kernel_hw, cpu_rng) {
        (Some(name), _) => Some(format!("a hardware random source ({name})")),
        (None, true) => Some("the processor's built-in random instruction".to_string()),
        (None, false) => None,
    };
    match (device_present, backing) {
        // The usual case in a container or on an ordinary computer: this
        // program cannot open the device, but the kernel it runs on is fed
        // by one. That is a sound source — just a single one.
        (false, Some(b)) => (
            "good",
            format!("Keys use the system generator, which this computer feeds from {b}. A second, direct hardware read is not available here (normal in a container), so adding your own randomness when you create an identity is worthwhile."),
        ),
        (true, Some(b)) => (
            "ok",
            format!("The hardware random-number device is present but did not answer a direct read. Keys use the system generator alone, which is still fed by {b}."),
        ),
        (_, None) => (
            "ok",
            "No hardware random source was found on this computer. Keys use the system generator alone — add your own randomness when you create an identity.".into(),
        ),
    }
}

fn probe() -> Health {
    let kernel_ready = kernel_ready();
    let hardware = read_trimmed(&format!("{HWRNG_SYS}/rng_current")).filter(|n| n != "none");
    let device_present = std::path::Path::new(HWRNG_DEV).exists();
    let hardware_ok = hardware_bytes(32).is_some();
    let cpu_rng = std::fs::read_to_string("/proc/cpuinfo").map(|c| cpu_has_rng(&c)).unwrap_or(false);
    let mut sources = Vec::new();
    if kernel_ready {
        sources.push("kernel");
        if hardware_ok {
            sources.push("hardware");
        }
    }
    let (verdict, summary) = classify(kernel_ready, device_present, hardware_ok, hardware.as_deref(), cpu_rng);
    Health {
        verdict,
        summary,
        kernel_ready,
        entropy_avail: read_trimmed("/proc/sys/kernel/random/entropy_avail").and_then(|s| s.parse().ok()),
        hardware,
        hardware_ok,
        cpu_rng,
        sources,
        recommend_personal: !hardware_ok,
    }
}

/// Current health. Cached for half a minute: the console polls, and there is
/// no reason to pull from the hardware generator on every refresh.
pub fn health() -> Health {
    static CACHE: OnceLock<Mutex<Option<(Instant, Health)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(g) = cache.lock() {
        if let Some((at, h)) = g.as_ref() {
            if at.elapsed() < Duration::from_secs(30) {
                return h.clone();
            }
        }
    }
    let h = probe();
    if let Ok(mut g) = cache.lock() {
        *g = Some((Instant::now(), h.clone()));
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_source_changes_the_result() {
        let (k, h, p) = ([1u8; 32], [2u8; 32], b"dice: 4 2 6 1");
        let base = mix(&k, Some(&h), p);
        assert_ne!(base, mix(&[9u8; 32], Some(&h), p));
        assert_ne!(base, mix(&k, Some(&[9u8; 32]), p));
        assert_ne!(base, mix(&k, Some(&h), b"dice: 4 2 6 2"));
        assert_ne!(base, mix(&k, None, p));
        assert_ne!(base, mix(&k, Some(&h), b""));
        // Deterministic for the same inputs (it is a hash, not a generator).
        assert_eq!(base, mix(&k, Some(&h), p));
    }

    #[test]
    fn inputs_cannot_run_into_each_other() {
        // Without length prefixes these two would hash the same bytes.
        assert_ne!(mix(b"ab", Some(b"c"), b""), mix(b"a", Some(b"bc"), b""));
        assert_ne!(mix(b"ab", None, b"c"), mix(b"ab", Some(b"c"), b""));
        assert_ne!(mix(b"", Some(b"x"), b""), mix(b"x", None, b""));
    }

    #[test]
    fn a_known_weak_source_cannot_weaken_the_key() {
        // The attacker knows the hardware bytes (all zero) and the personal
        // text; the kernel bytes alone still decide the outcome.
        let a = mix(&[7u8; 32], Some(&[0u8; 32]), b"1111");
        let b = mix(&[8u8; 32], Some(&[0u8; 32]), b"1111");
        assert_ne!(a, b);
    }

    #[test]
    fn a_stuck_generator_is_not_used() {
        assert!(!plausible(&[0u8; 32]));
        assert!(!plausible(&[0xFFu8; 32]));
        assert!(!plausible(&[1, 2, 1, 2, 1, 2, 1, 2, 1, 2, 1, 2, 1, 2, 1, 2, 1, 2, 1, 2, 1, 2, 1, 2, 1, 2, 1, 2, 1, 2, 1, 2]));
        assert!(!plausible(&[5u8; 4])); // too short to judge
        let varied: Vec<u8> = (0..32u8).map(|i| i.wrapping_mul(37).wrapping_add(11)).collect();
        assert!(plausible(&varied));
    }

    #[test]
    fn secrets_are_fresh_and_report_their_sources() {
        let a = strong_secret("").unwrap();
        let b = strong_secret("").unwrap();
        assert_ne!(a.bytes, b.bytes);
        assert_eq!(a.sources[0], "kernel");
        assert!(!a.sources.contains(&"personal"));
        let c = strong_secret("  three dice: 2 5 5  ").unwrap();
        assert!(c.sources.contains(&"personal"));
        // Whitespace-only input is not counted as a contribution.
        assert!(!strong_secret("   ").unwrap().sources.contains(&"personal"));
    }

    #[test]
    fn a_missing_device_is_reported_not_waited_on() {
        let t = Instant::now();
        assert!(read_device("/nonexistent/hwrng", 32, Duration::from_secs(1)).is_none());
        assert!(t.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn health_names_what_keys_are_made_from() {
        let h = probe();
        assert!(h.kernel_ready);
        assert!(matches!(h.verdict, "good" | "ok"));
        assert_eq!(h.sources.contains(&"hardware"), h.hardware_ok);
        assert_eq!(h.recommend_personal, !h.hardware_ok);
        assert!(!h.summary.is_empty());
    }

    #[test]
    fn every_kind_of_machine_gets_an_honest_verdict() {
        // A Pi: the chip is read directly.
        let (v, s) = classify(true, true, true, Some("107d208000.rng"), false);
        assert_eq!(v, "good");
        assert!(s.contains("read directly"));

        // A container on a desktop or in a VM: no device node, but the host
        // kernel is fed by hardware. Sound — and it must NOT be reported as a
        // chip that "did not answer".
        let (v, s) = classify(true, false, false, Some("virtio_rng.0"), true);
        assert_eq!(v, "good");
        assert!(s.contains("virtio_rng.0") && s.contains("container"));
        assert!(!s.contains("did not answer"));

        // A container where only the processor's instruction is visible.
        let (v, s) = classify(true, false, false, None, true);
        assert_eq!(v, "good");
        assert!(s.contains("processor"));

        // The device exists but a direct read failed: say so, and say what
        // still backs the generator.
        let (v, s) = classify(true, true, false, Some("tpm-rng-0"), false);
        assert_eq!(v, "ok");
        assert!(s.contains("did not answer") && s.contains("tpm-rng-0"));

        // Nothing at all: usable, but the person should add their own.
        let (v, s) = classify(true, false, false, None, false);
        assert_eq!(v, "ok");
        assert!(s.contains("add your own randomness"));

        // Not seeded yet overrides everything.
        assert_eq!(classify(false, true, true, Some("x"), true).0, "not_ready");
    }

    #[test]
    fn processor_random_instructions_are_recognised() {
        assert!(cpu_has_rng("processor\t: 0\nflags\t\t: fpu vme sse2 rdrand lahf_lm\n"));
        assert!(cpu_has_rng("flags : fpu rdseed adx\n"));
        assert!(cpu_has_rng("Features\t: fp asimd aes sha2 rng\n"));
        // "rng" inside another word, or on another line, does not count.
        assert!(!cpu_has_rng("Features\t: fp asimd aes sha2 crc32\n"));
        assert!(!cpu_has_rng("model name : Strange rng-ish CPU\nflags : fpu vme\n"));
        assert!(!cpu_has_rng(""));
    }
}
