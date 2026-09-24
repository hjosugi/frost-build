//! Convergence after a build is interrupted or its state is damaged (#149).
//!
//! Each test drives the real `frost` binary into one failure — SIGKILL in the
//! middle of a build, a write that fails because the disk is full, bytes of
//! `.frost/` truncated or flipped — and then asks the same question: does the
//! next ordinary build produce exactly what a from-scratch build of the same
//! sources produces, and is the build after that a no-op? Returning to the
//! right answer is the property; how much work it costs is not asserted,
//! except where losing already-journalled work would be a regression.
//!
//! The workspace is POSIX-shell genrules plus one owned output tree, so the
//! file is Unix-only. Real ENOSPC needs a small filesystem, which an
//! unprivileged test cannot mount: it runs when `FROST_TEST_ENOSPC_DIR` names
//! a directory on one (the `recovery` job in `.github/workflows/quality.yml`
//! mounts a tmpfs). The `RLIMIT_FSIZE` case runs everywhere and exercises the
//! same failed-write paths with `EFBIG`.
#![cfg(unix)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const LEAVES: usize = 24;
const GROUP: usize = 6;
const TREE_FILES: usize = 40;
const BIG_BYTES: usize = 1 << 20;

fn frost_bin() -> &'static str {
    env!("CARGO_BIN_EXE_frost")
}

fn manifest() -> String {
    let mut text = String::from(
        "[workspace]\ndefault_targets = [\"all\"]\n\n[toolchain.tools]\nsh = \"sh\"\n\n",
    );
    for leaf in 0..LEAVES {
        text.push_str(&format!(
            "[target.a{leaf:02}]\nkind = \"genrule\"\ncmd = \"cat ${{in}} | cksum > ${{out}}\"\n\
             inputs = [\"src/a{leaf:02}.txt\"]\noutputs = [\"out/a{leaf:02}.sum\"]\n\n"
        ));
    }
    let mut mids = Vec::new();
    for group in 0..LEAVES / GROUP {
        let members: Vec<String> = (group * GROUP..(group + 1) * GROUP)
            .map(|leaf| format!("a{leaf:02}"))
            .collect();
        let reads: Vec<String> = members.iter().map(|m| format!("${{dep:{m}}}")).collect();
        text.push_str(&format!(
            "[target.mid{group}]\nkind = \"genrule\"\ncmd = \"cat {} | cksum > ${{out}}\"\n\
             deps = [{}]\noutputs = [\"out/mid{group}.sum\"]\n\n",
            reads.join(" "),
            members
                .iter()
                .map(|m| format!("\"{m}\""))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        mids.push(format!("mid{group}"));
    }
    // Large enough that publishing it to the CAS is a write of its own that
    // can fail after the command itself succeeded.
    text.push_str(&format!(
        "[target.big]\nkind = \"genrule\"\n\
         cmd = \"head -c {BIG_BYTES} /dev/zero | tr '\\\\0' x > ${{out}} && cat ${{in}} >> ${{out}}\"\n\
         inputs = [\"src/big.txt\"]\noutputs = [\"out/big.bin\"]\n\n"
    ));
    // Blocks while `hold` exists, so a test can kill the build at a known
    // point: with this action running and every leaf already journalled.
    text.push_str(
        "[target.slow]\nkind = \"genrule\"\n\
         cmd = \"touch slow.started; while [ -f hold ]; do sleep 0.02; done; cat ${in} | cksum > ${out}\"\n\
         inputs = [\"src/slow.txt\"]\noutputs = [\"out/slow.sum\"]\n\n",
    );
    text.push_str(&format!(
        "[target.tree]\nkind = \"command\"\ntool = \"sh\"\n\
         args = [\"-c\", \"d=$1; mkdir -p \\\"$d\\\"; v=$(cksum < $2 | cut -d' ' -f1); i=0; \
         while [ $i -lt {TREE_FILES} ]; do echo $i $v > \\\"$d/f$i.txt\\\"; i=$((i+1)); done\", \
         \"fill\", \"${{output_dir}}\", \"${{in}}\"]\n\
         inputs = [\"src/tree.txt\"]\noutput_dirs = [\"trees/${{config}}/tree\"]\n\n"
    ));
    let mut reads: Vec<String> = mids.iter().map(|m| format!("${{dep:{m}}}")).collect();
    reads.push("${dep:big}".into());
    reads.push("${dep:slow}".into());
    let mut deps: Vec<String> = mids.iter().map(|m| format!("\"{m}\"")).collect();
    deps.extend(["\"big\"".into(), "\"slow\"".into(), "\"tree\"".into()]);
    text.push_str(&format!(
        "[target.all]\nkind = \"genrule\"\ncmd = \"cat {} | cksum > ${{out}}\"\n\
         deps = [{}]\noutputs = [\"out/all.sum\"]\n",
        reads.join(" "),
        deps.join(", ")
    ));
    text
}

/// Every action the manifest above declares.
const ACTIONS: usize = LEAVES + LEAVES / GROUP + 4;

type Files = BTreeMap<String, Vec<u8>>;

struct Workspace {
    dir: PathBuf,
    /// Clean-build outputs by the source tree that produced them, so a test
    /// that returns to the same sources does not rebuild its oracle.
    references: RefCell<BTreeMap<Files, Files>>,
}

impl Workspace {
    fn new(name: &str) -> Self {
        Self::at(std::env::temp_dir().join(format!("frost-recovery-{name}-{}", std::process::id())))
    }

    fn at(dir: PathBuf) -> Self {
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("frost.toml"), manifest()).unwrap();
        for leaf in 0..LEAVES {
            std::fs::write(
                dir.join(format!("src/a{leaf:02}.txt")),
                format!("leaf {leaf}\n"),
            )
            .unwrap();
        }
        for name in ["big", "slow", "tree"] {
            std::fs::write(dir.join(format!("src/{name}.txt")), format!("{name}\n")).unwrap();
        }
        Self {
            dir,
            references: RefCell::new(BTreeMap::new()),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(frost_bin());
        command
            .arg("-C")
            .arg(&self.dir)
            .args(args)
            .env("XDG_CONFIG_HOME", self.dir.join(".no-user-config"))
            .stdin(Stdio::null());
        command
    }

    /// Build, returning success, the number of actions that executed, and the
    /// combined output for assertion messages.
    fn build(&self) -> (bool, usize, String) {
        self.build_with(|_| {})
    }

    fn build_with(&self, configure: impl FnOnce(&mut Command)) -> (bool, usize, String) {
        let events = self.dir.with_extension("events.ndjson");
        let _ = std::fs::remove_file(&events);
        let mut command = self.command(&[
            &format!("--build-event-json={}", events.display()),
            "build",
            "--no-tui",
            "-j",
            "4",
        ]);
        configure(&mut command);
        let out = command.output().expect("spawn frost");
        let text = String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr);
        assert!(
            !text.contains("panicked at"),
            "frost panicked instead of failing cleanly:\n{text}"
        );
        let executed = std::fs::read_to_string(&events)
            .unwrap_or_default()
            .lines()
            .filter(|line| {
                line.contains("\"event\":\"action_finished\"")
                    && (line.contains("\"result\":\"executed\"")
                        || line.contains("\"result\":\"flaky\""))
            })
            .count();
        let _ = std::fs::remove_file(&events);
        (out.status.success(), executed, text)
    }

    fn write(&self, rel: &str, content: &str) {
        std::fs::write(self.dir.join(rel), content).unwrap();
    }

    /// Every declared output and every file of the owned tree.
    fn outputs(&self) -> BTreeMap<String, Vec<u8>> {
        let mut files = BTreeMap::new();
        for dir in ["out", "trees"] {
            collect(&self.dir, &self.dir.join(dir), &mut files);
        }
        files
    }

    /// A from-scratch build of this workspace's current sources.
    fn reference(&self, name: &str) -> Files {
        let mut sources = Files::new();
        collect(&self.dir, &self.dir.join("src"), &mut sources);
        if let Some(outputs) = self.references.borrow().get(&sources) {
            return outputs.clone();
        }
        let fresh = Workspace::new(name);
        for entry in std::fs::read_dir(self.dir.join("src")).unwrap() {
            let entry = entry.unwrap();
            std::fs::copy(entry.path(), fresh.dir.join("src").join(entry.file_name())).unwrap();
        }
        let (ok, executed, out) = fresh.build();
        assert!(ok, "reference build failed:\n{out}");
        assert_eq!(
            executed, ACTIONS,
            "reference build ran {executed} actions:\n{out}"
        );
        let outputs = fresh.outputs();
        self.references
            .borrow_mut()
            .insert(sources, outputs.clone());
        outputs
    }

    /// The property every test ends on: an ordinary build succeeds, its
    /// outputs equal a clean build's byte for byte, and the build after it
    /// runs nothing.
    fn assert_converges(&self, context: &str) -> usize {
        let (ok, executed, out) = self.build();
        assert!(ok, "{context}: the recovery build failed:\n{out}");
        let reference = self.reference(&format!("reference-{}", sanitize(context)));
        let actual = self.outputs();
        let missing: Vec<_> = reference
            .keys()
            .filter(|k| !actual.contains_key(*k))
            .collect();
        let extra: Vec<_> = actual
            .keys()
            .filter(|k| !reference.contains_key(*k))
            .collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "{context}: output set differs from a clean build; missing {missing:?}, \
             unexpected {extra:?}\n{out}"
        );
        for (path, bytes) in &reference {
            assert!(
                actual.get(path) == Some(bytes),
                "{context}: {path} differs from a clean build"
            );
        }
        let (ok, again, out) = self.build();
        assert!(
            ok && again == 0,
            "{context}: build after recovery was not a no-op ({again} ran):\n{out}"
        );
        executed
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn sanitize(text: &str) -> String {
    text.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn collect(root: &Path, dir: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect(root, &path, files);
        } else {
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            files.insert(rel, std::fs::read(&path).unwrap());
        }
    }
}

/// A tiny deterministic generator, so a failing case names its seed.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.0 >> 33
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

fn spawn_build_in_own_group(ws: &Workspace) -> std::process::Child {
    ws.command(&["build", "--no-tui", "-j", "4"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn()
        .expect("spawn frost")
}

/// SIGKILL frost and every command it started, as a crash or OOM kill would.
fn kill_group(child: &mut std::process::Child) {
    // SAFETY: plain syscall on a process group this test created.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.wait();
}

fn journalled(ws: &Workspace) -> usize {
    frostbuild_core::journal::Journal::load(&ws.dir)
        .actions
        .len()
}

#[test]
fn sigkill_while_an_action_runs_keeps_finished_work_and_converges() {
    let ws = Workspace::new("kill-held");
    ws.write("hold", "");
    let mut child = spawn_build_in_own_group(&ws);
    let deadline = Instant::now() + Duration::from_secs(60);
    // Every action except `slow` (held) and `all` (waits on it) can finish.
    while !(ws.dir.join("slow.started").exists() && journalled(&ws) >= ACTIONS - 2) {
        assert!(
            Instant::now() < deadline,
            "build never reached the held action"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    kill_group(&mut child);
    std::fs::remove_file(ws.dir.join("hold")).unwrap();

    let executed = ws.assert_converges("sigkill while slow ran");
    assert_eq!(
        executed, 2,
        "only the killed action and its dependent should rerun; journalled work was lost"
    );
}

#[test]
fn sigkill_at_arbitrary_moments_always_converges() {
    let ws = Workspace::new("kill-anywhere");
    let mut rng = Lcg(0x149);
    // Kills land during startup, graph compilation, scheduling, command
    // execution, output publication and journal writes, depending on the
    // delay and the host; the assertion is the same wherever they land.
    for (round, delay_ms) in [0u64, 2, 5, 10, 20, 35, 50, 80, 120, 200, 300, 450]
        .into_iter()
        .enumerate()
    {
        for _ in 0..3 {
            let pick = rng.below(LEAVES + 3);
            let rel = match pick {
                n if n < LEAVES => format!("src/a{n:02}.txt"),
                n if n == LEAVES => "src/big.txt".into(),
                n if n == LEAVES + 1 => "src/tree.txt".into(),
                _ => "src/slow.txt".into(),
            };
            ws.write(&rel, &format!("round {round} value {}\n", rng.next()));
        }
        let mut child = spawn_build_in_own_group(&ws);
        std::thread::sleep(Duration::from_millis(delay_ms));
        kill_group(&mut child);
    }
    ws.assert_converges("sigkill at arbitrary moments");
}

#[test]
fn a_torn_journal_tail_after_a_kill_is_ignored() {
    let ws = Workspace::new("torn-tail");
    let (ok, _, out) = ws.build();
    assert!(ok, "{out}");
    ws.write("src/a03.txt", "changed\n");
    // A frame whose length promises more bytes than follow: what a kill in
    // the middle of an append leaves behind.
    let journal = ws.dir.join(".frost/journal.bin");
    let mut bytes = std::fs::read(&journal).unwrap();
    bytes.extend_from_slice(&200u32.to_le_bytes());
    bytes.extend_from_slice(b"partial record");
    std::fs::write(&journal, bytes).unwrap();
    let executed = ws.assert_converges("torn journal tail");
    assert_eq!(executed, 3, "a03, its group and the root should rerun");
}

/// Corruptions of one state file. Each is applied to a copy of the state a
/// successful build left, then the workspace must converge — and must still
/// converge after a later source edit, which is what catches damaged state
/// that is *trusted* rather than rebuilt.
type Mutation = (&'static str, Box<dyn Fn(&mut Vec<u8>)>);

fn mutations(len: usize) -> Vec<Mutation> {
    let mut list: Vec<Mutation> = vec![
        ("emptied", Box::new(|b: &mut Vec<u8>| b.clear())),
        (
            "truncated to half",
            Box::new(|b: &mut Vec<u8>| b.truncate(b.len() / 2)),
        ),
        (
            "last byte dropped",
            Box::new(|b: &mut Vec<u8>| {
                b.pop();
            }),
        ),
        (
            "magic damaged",
            Box::new(|b: &mut Vec<u8>| {
                if let Some(first) = b.first_mut() {
                    *first ^= 0xff;
                }
            }),
        ),
        (
            "garbage",
            Box::new(|b: &mut Vec<u8>| {
                let mut rng = Lcg(b.len() as u64);
                for byte in b.iter_mut() {
                    *byte = rng.next() as u8;
                }
            }),
        ),
    ];
    // Single-bit flips spread over the body: the damage a bad sector or a
    // torn page does, and the kind a format without a checksum can decode
    // into something plausible.
    let positions: Vec<usize> = if len <= 8 {
        Vec::new()
    } else {
        let mut rng = Lcg(len as u64 ^ 0x5eed);
        (0..6).map(|_| 8 + rng.below(len - 8)).collect()
    };
    for position in positions {
        list.push((
            "bit flipped",
            Box::new(move |b: &mut Vec<u8>| {
                if position < b.len() {
                    b[position] ^= 0x10;
                }
            }),
        ));
    }
    list
}

fn copy_tree(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let target = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn corrupt_and_converge(state_file: &str) {
    let ws = Workspace::new(&format!("corrupt-{}", sanitize(state_file)));
    let (ok, _, out) = ws.build();
    assert!(ok, "{out}");
    // Build once more so the no-op certificate exists too.
    let (ok, _, out) = ws.build();
    assert!(ok, "{out}");
    let pristine = ws.dir.with_extension("pristine");
    let _ = std::fs::remove_dir_all(&pristine);
    copy_tree(&ws.dir.join(".frost"), &pristine);
    let path = ws.dir.join(".frost").join(state_file);
    let original = std::fs::read(&path)
        .unwrap_or_else(|error| panic!("{state_file} was not written by the build: {error}"));

    for (index, (name, mutate)) in mutations(original.len()).into_iter().enumerate() {
        let _ = std::fs::remove_dir_all(ws.dir.join(".frost"));
        copy_tree(&pristine, &ws.dir.join(".frost"));
        // Undo the previous round's source edit so every round starts from
        // the state the pristine copy describes.
        ws.write("src/a07.txt", "leaf 7\n");
        let (ok, _, out) = ws.build();
        assert!(ok, "{out}");
        let mut bytes = original.clone();
        mutate(&mut bytes);
        std::fs::write(&path, &bytes).unwrap();
        let context = format!("{state_file} {name} (case {index})");
        ws.assert_converges(&context);
        ws.write("src/a07.txt", "edited after the damage\n");
        let (ok, executed, out) = ws.build();
        assert!(ok, "{context}, then an edit: {out}");
        assert!(
            executed >= 3,
            "{context}: an edited leaf did not rebuild ({executed} ran)"
        );
        ws.assert_converges(&format!("{context}, then an edit"));
    }
    let _ = std::fs::remove_dir_all(&pristine);
}

#[test]
fn a_damaged_journal_costs_work_never_correctness() {
    corrupt_and_converge("journal.bin");
}

#[test]
fn a_damaged_hash_cache_costs_work_never_correctness() {
    corrupt_and_converge("hashcache.bin");
}

#[test]
fn a_damaged_graph_store_costs_work_never_correctness() {
    corrupt_and_converge("graph-debug.bin");
}

#[test]
fn a_damaged_noop_certificate_costs_work_never_correctness() {
    corrupt_and_converge("noop-debug.bin");
}

#[test]
fn a_damaged_toolchain_stamp_costs_work_never_correctness() {
    corrupt_and_converge("toolchain.bin");
}

#[test]
fn damaged_cas_objects_are_never_restored() {
    let ws = Workspace::new("corrupt-cas");
    let (ok, _, out) = ws.build();
    assert!(ok, "{out}");
    let mut objects = Vec::new();
    fn walk(dir: &Path, found: &mut Vec<PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, found);
            } else {
                found.push(path);
            }
        }
    }
    walk(&ws.dir.join(".frost/cas"), &mut objects);
    assert!(
        !objects.is_empty(),
        "the build published nothing to the CAS"
    );
    for object in &objects {
        let mut bytes = std::fs::read(object).unwrap();
        if bytes.is_empty() {
            continue;
        }
        let middle = bytes.len() / 2;
        bytes[middle] ^= 0x01;
        std::fs::write(object, bytes).unwrap();
    }
    // With the outputs gone, restoring from the CAS is the cheap path; every
    // object behind it is now wrong.
    std::fs::remove_dir_all(ws.dir.join("out")).unwrap();
    std::fs::remove_dir_all(ws.dir.join("trees")).unwrap();
    ws.assert_converges("every CAS object damaged, outputs deleted");
}

#[test]
fn random_damage_anywhere_in_frost_state_converges() {
    let ws = Workspace::new("corrupt-random");
    let (ok, _, out) = ws.build();
    assert!(ok, "{out}");
    let mut rng = Lcg(0xd15c);
    for round in 0..6 {
        let mut files = BTreeMap::new();
        collect(&ws.dir, &ws.dir.join(".frost"), &mut files);
        for (rel, bytes) in files {
            if bytes.is_empty() || rng.below(3) != 0 {
                continue;
            }
            let mut bytes = bytes;
            match rng.below(3) {
                0 => bytes.truncate(rng.below(bytes.len())),
                1 => {
                    let at = rng.below(bytes.len());
                    bytes[at] ^= 1 << rng.below(8);
                }
                _ => bytes.extend_from_slice(b"\x05\x00\x00\x00trail"),
            }
            std::fs::write(ws.dir.join(&rel), bytes).unwrap();
        }
        ws.write(
            &format!("src/a{:02}.txt", rng.below(LEAVES)),
            &format!("round {round}\n"),
        );
        ws.assert_converges(&format!("random damage round {round}"));
    }
}

/// Run `configure`d builds that are expected to hit a failing write, then
/// prove that removing the cause lets the next build converge.
fn failed_writes_converge(
    ws: &Workspace,
    label: &str,
    rounds: &[u64],
    mut constrained_build: impl FnMut(&Workspace, u64) -> (bool, usize, String),
    mut release: impl FnMut(&Workspace),
    error_text: &str,
) {
    // Where each failing build reported its error, so a CI log shows which
    // write paths the sizes below actually reached.
    let mut failures: Vec<String> = Vec::new();
    let mut record = |phase: &str, limit: u64, out: &str| {
        assert!(
            out.contains(error_text),
            "{label} {limit} ({phase}): failed without reporting the write error:\n{out}"
        );
        let line = out
            .lines()
            .find(|line| line.contains(error_text))
            .unwrap_or_default()
            .trim()
            .to_string();
        failures.push(format!("{label} {limit} ({phase}): {line}"));
    };
    for &limit in rounds {
        // Start each round cold, so every write path (commands, CAS
        // publication, journal, hash cache, graph store) is exercised under
        // the limit rather than only the ones an incremental build touches.
        let _ = std::fs::remove_dir_all(ws.dir.join(".frost"));
        let _ = std::fs::remove_dir_all(ws.dir.join("out"));
        let _ = std::fs::remove_dir_all(ws.dir.join("trees"));
        let (ok, _, out) = constrained_build(ws, limit);
        if !ok {
            record("cold", limit, &out);
        }
        release(ws);
        ws.assert_converges(&format!("{label} {limit}"));

        // And warm: a change whose rebuild hits the limit part-way.
        ws.write("src/big.txt", &format!("big after {limit}\n"));
        ws.write("src/a11.txt", &format!("a11 after {limit}\n"));
        let (ok, _, out) = constrained_build(ws, limit);
        if !ok {
            record("warm", limit, &out);
        }
        release(ws);
        ws.assert_converges(&format!("{label} {limit} (warm)"));
    }
    assert!(
        !failures.is_empty(),
        "{label}: no round failed, so nothing was tested"
    );
    for failure in &failures {
        eprintln!("{failure}");
    }
}

#[test]
fn builds_that_exceed_the_file_size_limit_converge() {
    let ws = Workspace::new("fsize");
    // RLIMIT_FSIZE makes any write past the limit fail with EFBIG (SIGXFSZ is
    // ignored so the writer sees the error instead of dying). The smallest
    // limits stop frost's own graph and state writes before any command runs;
    // the middle ones stop the command writing the 1 MiB output; the largest
    // lets every write through, the control that shows the limit is the
    // cause.
    failed_writes_converge(
        &ws,
        "RLIMIT_FSIZE",
        &[2 << 10, 4 << 10, 16 << 10, 64 << 10, 512 << 10, 3 << 20],
        |ws, limit| {
            ws.build_with(|command| {
                // SAFETY: setrlimit and signal are async-signal-safe and
                // touch only the child between fork and exec.
                unsafe {
                    command.pre_exec(move || {
                        let rlimit = libc::rlimit {
                            rlim_cur: limit as libc::rlim_t,
                            rlim_max: limit as libc::rlim_t,
                        };
                        if libc::setrlimit(libc::RLIMIT_FSIZE, &rlimit) != 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                        libc::signal(libc::SIGXFSZ, libc::SIG_IGN);
                        Ok(())
                    });
                }
            })
        },
        |_| {},
        "File too large",
    );
}

#[cfg(target_os = "linux")]
fn available_bytes(dir: &Path) -> u64 {
    use std::os::unix::ffi::OsStrExt;
    let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).unwrap();
    // SAFETY: statvfs writes into the zeroed struct we own.
    unsafe {
        let mut stat: libc::statvfs = std::mem::zeroed();
        assert_eq!(libc::statvfs(path.as_ptr(), &mut stat), 0);
        stat.f_bavail as u64 * stat.f_frsize as u64
    }
}

#[cfg(target_os = "linux")]
#[test]
fn builds_on_a_full_filesystem_converge_once_space_returns() {
    let Some(mount) = std::env::var_os("FROST_TEST_ENOSPC_DIR") else {
        eprintln!(
            "skipped: set FROST_TEST_ENOSPC_DIR to a directory on a small filesystem \
             (for example a 16 MiB tmpfs) to run the real ENOSPC case"
        );
        return;
    };
    let mount = PathBuf::from(mount);
    let ws = Workspace::at(mount.join("frost-recovery-enospc"));
    let filler = mount.join("filler");
    // Leave this much space free, then build. The same sizes as the rlimit
    // case: inside the command, and after it in frost's own writes.
    failed_writes_converge(
        &ws,
        "ENOSPC",
        &[
            16 << 10,
            256 << 10,
            1 << 20,
            (1 << 20) + (256 << 10),
            3 << 20,
        ],
        |ws, free| {
            let _ = std::fs::remove_file(&filler);
            let available = available_bytes(&mount);
            let fill = available.saturating_sub(free);
            let file = std::fs::File::create(&filler).unwrap();
            // Real blocks, not a sparse file: write zeros.
            let chunk = vec![0u8; 1 << 16];
            let mut written = 0u64;
            let mut writer = std::io::BufWriter::new(file);
            use std::io::Write;
            while written < fill {
                let n = (fill - written).min(chunk.len() as u64) as usize;
                if writer.write_all(&chunk[..n]).is_err() {
                    break;
                }
                written += n as u64;
            }
            let _ = writer.flush();
            ws.build()
        },
        |_| {
            let _ = std::fs::remove_file(&filler);
        },
        "No space left on device",
    );
}

/// Run frost in a user namespace whose inotify watch limit is `limit`, or
/// `None` when this host does not allow unprivileged user namespaces.
#[cfg(target_os = "linux")]
fn frost_with_watch_limit(ws: &Workspace, limit: u32, args: &[&str]) -> Option<(i32, String)> {
    let probe = Command::new("unshare")
        .args([
            "-Ur",
            "sh",
            "-c",
            "echo 1 > /proc/sys/user/max_inotify_watches",
        ])
        .output()
        .ok()?;
    if !probe.status.success() {
        return None;
    }
    let script = format!("echo {limit} > /proc/sys/user/max_inotify_watches && exec \"$@\"");
    let mut command = Command::new("unshare");
    command
        .args(["-Ur", "sh", "-c", &script, "sh", frost_bin(), "-C"])
        .arg(&ws.dir)
        .args(args)
        .env("XDG_CONFIG_HOME", ws.dir.join(".no-user-config"))
        .stdin(Stdio::null());
    let out = command.output().expect("spawn unshare");
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    Some((out.status.code().unwrap_or(-1), text))
}

#[cfg(target_os = "linux")]
#[test]
fn past_the_watch_limit_daemon_builds_fall_back_to_building_in_process() {
    let ws = Workspace::new("watch-limit");
    // More directories than the limit below, as a monorepo has more than a
    // default `fs.inotify.max_user_watches`.
    for index in 0..48 {
        std::fs::create_dir_all(ws.dir.join(format!("extra/d{index:02}"))).unwrap();
    }
    let Some((code, out)) = frost_with_watch_limit(&ws, 16, &["build", "--daemon", "--no-tui"])
    else {
        eprintln!("skipped: unprivileged user namespaces are not available on this host");
        return;
    };
    assert_eq!(
        code, 0,
        "a daemon that cannot watch must not fail the build:\n{out}"
    );
    assert!(out.contains("building without the daemon"), "{out}");
    ws.assert_converges("daemon past the watch limit");

    // `frost watch` has no fallback worth having — watching is its whole job
    // — so it must say why it stopped rather than sit idle.
    let (code, out) = frost_with_watch_limit(&ws, 16, &["watch", "--debounce-ms", "20"]).unwrap();
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("watch limit"), "{out}");
}
