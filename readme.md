# pdfdiff

Produces a PDF from two LaTeX sources with all changes marked in red — additions in red, deletions in red strikethrough.

Wraps [`latexdiff`](https://ctan.org/pkg/latexdiff) and handles the full build pipeline: bibliography generation, multi-file projects, and arXiv-style embedded `.bbl` files.

## Usage

```
pdfdiff <original> <modified> <output-dir>
```

Each source can be:

| Form | Example |
|------|---------|
| Git branch name | `main` |
| `.tex` file path | `paper/main.tex` |
| Project directory | `paper/` |

For directories and branches, the file containing `\documentclass` is used as the root automatically.

`diff.tex` and `diff.pdf` are written to `<output-dir>`.

### Examples

```sh
# Compare two git branches
pdfdiff main feature-branch out/

# Compare two directories
pdfdiff paper-v1/ paper-v2/ out/

# Compare specific .tex files
pdfdiff old/main.tex new/main.tex out/

# Mix and match
pdfdiff main paper-v2/ out/
```

## Requirements

Required:

- [`latexdiff`](https://ctan.org/pkg/latexdiff) — usually part of TeX Live / MiKTeX
- `pdflatex`
- `git` (for branch inputs)

Optional but recommended:

- `latexpand` — enables `--flatten` so `\input`/`\include` directives are inlined; ships with TeX Live
- `bibtex` / `biber` — required if the project has a bibliography

## Installation

```sh
cargo install pdfdiff
```

## How it works

1. Each source is resolved to a root `.tex` file (extracting git branches via `git archive` into a temp directory as needed).
2. The modified project is copied into `<output-dir>` so all relative asset paths work.
3. The modified source is pre-built (`pdflatex` + bibliography tool) to generate `.bbl` and other artifacts before the diff is compiled.
4. `latexdiff` produces `diff.tex` with change markup.
5. Red colour overrides are injected: both additions and deletions render in red (deletions get a strikethrough via `\sout`).
6. `diff.tex` is compiled twice with `pdflatex` to resolve cross-references.

### arXiv projects

Projects that embed `\input{main.bbl}` instead of a live `\bibliography{}` call are handled automatically: a minimal `.aux` stub is written and `bibtex` is run to synthesise the missing `.bbl` before compilation.
