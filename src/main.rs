use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{bail, Context, Result};
use clap::Parser;
use tempfile::TempDir;
use walkdir::WalkDir;

/// Produce a PDF with LaTeX changes marked in red.
///
/// Each SOURCE can be a git branch name, a .tex file path, or a project directory.
/// For directories and branches, the file containing \documentclass is used as the root.
#[derive(Parser)]
#[command(name = "pdfdiff", version)]
struct Args {
    /// Original version: git branch, .tex file, or project directory
    original: String,
    /// Modified version: git branch, .tex file, or project directory
    modified: String,
    /// Output directory — diff.tex and diff.pdf are written here
    output: PathBuf,
}

enum Source {
    GitBranch(String),
    File(PathBuf),
    Directory(PathBuf),
}

fn detect_source(arg: &str) -> Result<Source> {
    let path = Path::new(arg);
    if path.is_file() {
        return Ok(Source::File(path.to_path_buf()));
    }
    if path.is_dir() {
        return Ok(Source::Directory(path.to_path_buf()));
    }
    let out = Command::new("git")
        .args(["rev-parse", "--verify", arg])
        .stderr(Stdio::null())
        .output()
        .context("failed to run git — is it installed?")?;
    if out.status.success() {
        return Ok(Source::GitBranch(arg.to_string()));
    }
    bail!("'{}' is not a file, directory, or git branch", arg);
}

fn extract_git_branch(branch: &str, dest: &Path) -> Result<()> {
    let archive = Command::new("git")
        .args(["archive", branch])
        .output()
        .with_context(|| format!("git archive failed for branch '{}'", branch))?;
    if !archive.status.success() {
        bail!(
            "git archive '{}' failed: {}",
            branch,
            String::from_utf8_lossy(&archive.stderr).trim()
        );
    }
    let dest_str = dest.to_str().context("temp dir path is not valid UTF-8")?;
    let mut tar = Command::new("tar")
        .args(["-x", "-C", dest_str])
        .stdin(Stdio::piped())
        .spawn()
        .context("failed to spawn tar")?;
    tar.stdin
        .as_mut()
        .context("tar stdin not available")?
        .write_all(&archive.stdout)
        .context("piping archive to tar")?;
    if !tar.wait()?.success() {
        bail!("tar extraction failed");
    }
    Ok(())
}

fn find_main_tex(dir: &Path) -> Result<PathBuf> {
    let mut candidates: Vec<PathBuf> = WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "tex").unwrap_or(false))
        .filter_map(|e| {
            let content = std::fs::read_to_string(e.path()).ok()?;
            // Exclude latexdiff-generated files
            if content.contains("%DIF PREAMBLE EXTENSION ADDED BY LATEXDIFF") {
                return None;
            }
            content.contains(r"\documentclass").then(|| e.path().to_path_buf())
        })
        .collect();

    match candidates.len() {
        0 => bail!(
            "no .tex file with \\documentclass found in {}",
            dir.display()
        ),
        1 => Ok(candidates.remove(0)),
        _ => {
            // Prefer a file at the root of the directory
            if let Some(root) = candidates.iter().find(|p| p.parent() == Some(dir)) {
                return Ok(root.clone());
            }
            bail!(
                "multiple root .tex files found — pass a specific file instead:\n{}",
                candidates
                    .iter()
                    .map(|p| format!("  {}", p.display()))
                    .collect::<Vec<_>>()
                    .join("\n")
            )
        }
    }
}

/// Returns (main_tex_path, assets_dir_to_copy).
fn resolve_source(source: Source, tmp: &Path) -> Result<(PathBuf, Option<PathBuf>)> {
    match source {
        Source::File(p) => Ok((p, None)),
        Source::Directory(dir) => {
            let main = find_main_tex(&dir)?;
            Ok((main, Some(dir)))
        }
        Source::GitBranch(branch) => {
            extract_git_branch(&branch, tmp)?;
            let main = find_main_tex(tmp)?;
            Ok((main, Some(tmp.to_path_buf())))
        }
    }
}

fn copy_dir(src: &Path, dest: &Path) -> Result<()> {
    for entry in WalkDir::new(src).into_iter().filter_map(|e| e.ok()) {
        if !entry.file_type().is_file() {
            continue;
        }
        let rel = entry.path().strip_prefix(src)?;
        let target = dest.join(rel);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(entry.path(), &target)
            .with_context(|| format!("copying {}", entry.path().display()))?;
    }
    Ok(())
}

// Injected just before \begin{document} to override latexdiff's default colours.
// Both additions and deletions become red; deletions get a strikethrough via ulem's \sout
// (latexdiff loads ulem when --type=UNDERLINE is used, which is the default).
const RED_OVERRIDES: &str = concat!(
    "\n%% pdfdiff: force red for all tracked changes\n",
    "\\usepackage[normalem]{ulem}\n",
    "\\usepackage{color}\n",
    "\\renewcommand{\\DIFadd}[1]{{\\protect\\color{red}#1}}\n",
    "\\renewcommand{\\DIFdel}[1]{{\\protect\\color{red}\\sout{#1}}}\n",
    "\\providecommand{\\DIFadd}[1]{{\\protect\\color{red}#1}}\n",
    "\\providecommand{\\DIFdel}[1]{{\\protect\\color{red}\\sout{#1}}}\n",
);

fn inject_red(tex: String) -> String {
    if let Some(pos) = tex.find(r"\begin{document}") {
        let mut out = tex;
        out.insert_str(pos, RED_OVERRIDES);
        out
    } else {
        format!("{}{}", RED_OVERRIDES, tex)
    }
}

/// Scan all .tex files under `dir` and return lines matching the predicate,
/// stopping after the first hit (used to find \bibliographystyle / \bibliography).
fn find_in_tex_files<F>(dir: &Path, predicate: F) -> Option<String>
where
    F: Fn(&str) -> Option<String>,
{
    WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "tex").unwrap_or(false))
        .find_map(|e| {
            let content = std::fs::read_to_string(e.path()).ok()?;
            content.lines().find_map(|l| predicate(l))
        })
}

/// Extract the argument from a LaTeX command, e.g. `\foo{bar}` → `"bar"`.
/// Requires `{` immediately after the command name so `\bibliography` does not
/// accidentally match `\bibliographystyle`.
fn extract_arg(line: &str, cmd: &str) -> Option<String> {
    let search = format!("{cmd}{{");
    let start = line.find(&search)? + search.len();
    let end = line[start..].find('}')?;
    Some(line[start..start + end].trim().to_string())
}

/// For projects that embed `\input{main.bbl}` for arXiv submission (rather than keeping a
/// live `\bibliography{}` call), the .bbl file is not tracked in git. This function
/// synthesises the missing .bbl by constructing a minimal .aux file and running bibtex,
/// avoiding any need to patch or re-run pdflatex.
fn generate_missing_bbls(work_dir: &Path) -> Result<()> {
    // Collect every \input{*.bbl} reference whose target doesn't exist yet.
    let missing: Vec<String> = WalkDir::new(work_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "tex").unwrap_or(false))
        .flat_map(|e| {
            let content = std::fs::read_to_string(e.path()).unwrap_or_default();
            content
                .lines()
                .filter_map(|line| {
                    let inner = extract_arg(line.trim(), r"\input")?;
                    // Only care about explicit .bbl includes
                    let bbl = if inner.ends_with(".bbl") {
                        inner
                    } else {
                        return None;
                    };
                    (!work_dir.join(&bbl).exists()).then_some(bbl)
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let missing = {
        let mut seen = std::collections::HashSet::new();
        missing.into_iter().filter(|x| seen.insert(x.clone())).collect::<Vec<_>>()
    };

    if missing.is_empty() {
        return Ok(());
    }

    // Find the bibliography style (e.g. \bibliographystyle{ACM-Reference-Format}).
    let bibstyle = find_in_tex_files(work_dir, |l| {
        extract_arg(l, r"\bibliographystyle")
    })
    .unwrap_or_else(|| "plain".to_string());

    // Find the .bib database name — first try \bibliography{} (possibly commented out),
    // then fall back to any .bib file present in the directory.
    let bib_db = find_in_tex_files(work_dir, |l| {
        let stripped = l.trim().trim_start_matches('%').trim();
        extract_arg(stripped, r"\bibliography")
    })
    .or_else(|| {
        std::fs::read_dir(work_dir).ok()?.find_map(|e| {
            let e = e.ok()?;
            let path = e.path();
            if path.extension()? != "bib" { return None; }
            Some(path.file_stem()?.to_str()?.to_string())
        })
    });

    let Some(bib_db) = bib_db else {
        eprintln!(
            "warning: {} missing but no .bib file found — bibliography will be empty",
            missing.join(", ")
        );
        return Ok(());
    };

    for bbl in &missing {
        let stem = bbl.trim_end_matches(".bbl");
        println!("  generating {bbl} via bibtex ({bib_db}.bib / {bibstyle})...");

        // Write a minimal .aux that tells bibtex everything it needs.
        // \citation{*} is the bibtex equivalent of \nocite{*}: include all entries.
        let aux = format!(
            "\\relax\n\\bibdata{{{bib_db}}}\n\\bibstyle{{{bibstyle}}}\n\\citation{{*}}\n"
        );
        let aux_path = work_dir.join(format!("{stem}.aux"));
        std::fs::write(&aux_path, aux)
            .with_context(|| format!("writing stub {stem}.aux"))?;

        let status = Command::new("bibtex")
            .arg(stem)
            .current_dir(work_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .context("failed to run bibtex")?;

        // Remove the stub .aux so it doesn't interfere with the real build.
        let _ = std::fs::remove_file(&aux_path);

        if !status.success() {
            eprintln!("warning: bibtex reported errors generating {bbl}");
        }
    }

    Ok(())
}

fn run_pdflatex(work_dir: &Path, tex_file: &str, interactive: bool) -> bool {
    let mode = if interactive {
        "nonstopmode"
    } else {
        "batchmode"
    };
    Command::new("pdflatex")
        .args([&format!("-interaction={mode}"), tex_file])
        .current_dir(work_dir)
        .stdout(if interactive { Stdio::inherit() } else { Stdio::null() })
        .stderr(if interactive { Stdio::inherit() } else { Stdio::null() })
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Detect which bibliography tool the project uses by inspecting the .aux file.
/// Returns `Some("bibtex")`, `Some("biber")`, or `None` if no bibliography is needed.
fn detect_bib_tool(work_dir: &Path, stem: &str) -> Option<&'static str> {
    // biber leaves a .bcf file
    if work_dir.join(format!("{stem}.bcf")).exists() {
        return Some("biber");
    }
    // bibtex is indicated by \bibdata in the .aux file
    let aux = work_dir.join(format!("{stem}.aux"));
    if let Ok(content) = std::fs::read_to_string(&aux) {
        if content.contains(r"\bibdata") {
            return Some("bibtex");
        }
    }
    None
}

/// Pre-build the modified source to generate all build artifacts (.bbl, .ind, .gls, etc.)
/// before we compile diff.tex.  The sequence is:
///   1. Generate any missing .bbl files (handles arXiv-style \input{main.bbl} projects).
///   2. pdflatex pass — produces .aux and triggers bibtex/biber detection.
///   3. Run bibliography tool if needed (live-bibliography projects).
///   4. Second pdflatex pass — resolves cross-references.
fn prebuild(work_dir: &Path, main_tex: &str) -> Result<()> {
    let stem = Path::new(main_tex)
        .file_stem()
        .and_then(|s| s.to_str())
        .context("main tex file has no stem")?;

    // arXiv-style projects embed \input{main.bbl} and never call \bibliography{}.
    // Synthesise the missing .bbl(s) from the .bib file before pdflatex runs.
    generate_missing_bbls(work_dir)?;

    println!("  pre-build pass 1 (generating aux files)...");
    run_pdflatex(work_dir, main_tex, false);

    // Live-bibliography projects: detect bibtex/biber from the generated .aux/.bcf.
    if let Some(tool) = detect_bib_tool(work_dir, stem) {
        println!("  running {tool}...");
        let status = Command::new(tool)
            .arg(stem)
            .current_dir(work_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .with_context(|| format!("failed to run `{tool}`"))?;
        if !status.success() {
            eprintln!("warning: {tool} reported errors — bibliography may be incomplete");
        }
    }

    println!("  pre-build pass 2 (resolving references)...");
    run_pdflatex(work_dir, main_tex, false);

    Ok(())
}

fn run_latexdiff(old_tex: &Path, new_tex: &Path, out_tex: &Path) -> Result<()> {
    let mut cmd = Command::new("latexdiff");

    // --flatten inlines \input / \include so the diff is self-contained;
    // it requires latexpand on PATH.
    if which::which("latexpand").is_ok() {
        cmd.arg("--flatten");
    } else {
        eprintln!(
            "warning: `latexpand` not found; \\input/\\include directives will not be inlined.\n\
             Install it (usually shipped with TeX Live) for full multi-file support."
        );
    }

    let output = cmd
        .arg(old_tex)
        .arg(new_tex)
        .output()
        .context("failed to run `latexdiff`")?;

    if !output.status.success() {
        bail!(
            "latexdiff failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let diff =
        String::from_utf8(output.stdout).context("latexdiff output contains non-UTF-8 bytes")?;
    let patched = inject_red(diff);
    std::fs::write(out_tex, patched)
        .with_context(|| format!("writing {}", out_tex.display()))?;
    Ok(())
}

fn compile_pdf(work_dir: &Path) -> Result<PathBuf> {
    let pdf = work_dir.join("diff.pdf");
    for pass in 1..=2 {
        println!("  pdflatex pass {pass}/2...");
        let ok = run_pdflatex(work_dir, "diff.tex", true);
        if !ok {
            if pdf.exists() {
                eprintln!(
                    "warning: pdflatex reported errors on pass {pass} (see {}/diff.log)",
                    work_dir.display()
                );
            } else {
                bail!(
                    "pdflatex failed on pass {pass} and produced no output — see {}/diff.log",
                    work_dir.display()
                );
            }
        }
    }
    Ok(pdf)
}

fn require(tool: &str) -> Result<()> {
    which::which(tool)
        .with_context(|| format!("`{tool}` not found in PATH — please install it"))?;
    Ok(())
}

fn main() -> Result<()> {
    let args = Args::parse();

    require("latexdiff")?;
    require("pdflatex")?;

    let tmp_orig = TempDir::new().context("creating temp dir")?;
    let tmp_mod = TempDir::new().context("creating temp dir")?;

    println!("Resolving sources...");
    let orig = detect_source(&args.original)?;
    let modif = detect_source(&args.modified)?;

    let (orig_tex, orig_assets) = resolve_source(orig, tmp_orig.path())?;
    let (mod_tex, mod_assets) = resolve_source(modif, tmp_mod.path())?;

    println!("  original : {}", orig_tex.display());
    println!("  modified : {}", mod_tex.display());

    std::fs::create_dir_all(&args.output)
        .with_context(|| format!("creating output dir {}", args.output.display()))?;

    // Copy the modified project into the output dir so all relative paths
    // (\includegraphics, \input, etc.) resolve correctly at compile time.
    if let Some(assets) = mod_assets {
        println!("Copying project assets...");
        copy_dir(&assets, &args.output)?;
    }

    // Pre-build the modified source to generate all build artifacts (.bbl, .ind, .gls, …).
    // This runs pdflatex + the bibliography tool so that when we compile diff.tex every
    // file it might \input or reference already exists — no special-casing needed.
    if let Some(main_name) = mod_tex.file_name().and_then(|n| n.to_str()) {
        println!("Pre-building modified source (generating artifacts)...");
        prebuild(&args.output, main_name)?;
    }

    // Copy .bbl files from the output dir (which now has the correctly-built bibliography)
    // into the original source dir so that latexdiff --flatten can inline the same
    // bibliography on both sides. Without this the entire .bbl content shows as "added".
    if let Some(orig_dir) = orig_assets.as_deref().or_else(|| orig_tex.parent()) {
        for entry in WalkDir::new(&args.output)
            .max_depth(3)
            .into_iter()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "bbl").unwrap_or(false))
        {
            let rel = entry.path().strip_prefix(&args.output).unwrap();
            let dest = orig_dir.join(rel);
            if !dest.exists() {
                if let Some(p) = dest.parent() {
                    std::fs::create_dir_all(p)?;
                }
                std::fs::copy(entry.path(), &dest)?;
            }
        }
    }

    let diff_tex = args.output.join("diff.tex");
    println!("Running latexdiff...");
    run_latexdiff(&orig_tex, &mod_tex, &diff_tex)?;

    println!("Compiling diff PDF...");
    let pdf = compile_pdf(&args.output)?;

    println!("\nDone!  {}", pdf.display());
    Ok(())
}
