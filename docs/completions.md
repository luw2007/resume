# Shell completions

`resume` uses Clap's completion generator through the existing `completions bash|zsh|fish` subcommand. Generation prints a script to stdout:

```sh
resume completions bash
resume completions zsh
resume completions fish
```

The binary dispatches completion generation in `src/main.rs` before calling `app::run`. Consequently, these commands do **not** load a config file, derive Scope, or discover/scan Sessions.

## Bash

For the current user:

```sh
mkdir -p ~/.local/share/bash-completion/completions
resume completions bash > ~/.local/share/bash-completion/completions/resume
```

Start a new shell, or source the generated file directly. System installation locations vary by distribution.

## Zsh

Choose a directory already present in `fpath`, or add one:

```sh
mkdir -p ~/.zfunc
resume completions zsh > ~/.zfunc/_resume
```

Then ensure this appears before `compinit` in `~/.zshrc`:

```zsh
fpath=(~/.zfunc $fpath)
autoload -Uz compinit && compinit
```

## Fish

```sh
mkdir -p ~/.config/fish/completions
resume completions fish > ~/.config/fish/completions/resume.fish
```

Fish loads this file automatically in future shells.

## Repository-generated scripts

The checked-in copies under `completions/` are generated from the same Clap command definition:

```sh
cargo run --locked -- completions bash > completions/resume.bash
cargo run --locked -- completions zsh  > completions/_resume
cargo run --locked -- completions fish > completions/resume.fish
```

Regenerate them whenever CLI flags or subcommands change.
