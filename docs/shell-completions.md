# Shell completions

`dokploy completions` writes a completion script for Bash, Zsh, Fish,
PowerShell, or Elvish to standard output. It is an offline command: it does not
read a connection context, access credentials, or contact Dokploy.

Regenerate the script after upgrading the CLI so its commands and options match
the installed binary.

## Bash

Install the script in the per-user completion directory used by
`bash-completion`:

```bash
mkdir -p "${XDG_DATA_HOME:-$HOME/.local/share}/bash-completion/completions"
dokploy completions bash \
  > "${XDG_DATA_HOME:-$HOME/.local/share}/bash-completion/completions/dokploy"
```

Start a new shell after writing the file. The `bash-completion` package must be
installed and loaded by the shell.

## Zsh

Create a private completion directory and place it on `fpath` before calling
`compinit`:

```zsh
completion_directory="${ZDOTDIR:-$HOME}/.zfunc"
mkdir -p "$completion_directory"
dokploy completions zsh > "$completion_directory/_dokploy"
```

Add the following lines to `.zshrc`, then start a new shell:

```zsh
fpath=("${ZDOTDIR:-$HOME}/.zfunc" $fpath)
autoload -Uz compinit
compinit
```

## Fish

Write the generated script to Fish's per-user completion directory:

```fish
mkdir -p ~/.config/fish/completions
dokploy completions fish > ~/.config/fish/completions/dokploy.fish
```

Fish loads the file automatically in new shell sessions.

## PowerShell

Keep the generated script beside the PowerShell profile and load it from that
profile:

```powershell
$CompletionDirectory = Join-Path (Split-Path $PROFILE) "completions"
$CompletionFile = Join-Path $CompletionDirectory "dokploy.ps1"
New-Item -ItemType Directory -Force $CompletionDirectory | Out-Null
dokploy completions powershell | Set-Content -Encoding utf8 $CompletionFile
Add-Content $PROFILE ". '$CompletionFile'"
```

Run the final `Add-Content` command only once. Start a new PowerShell session
afterward.

## Elvish

Load the generated completion definition from `rc.elv`:

```elvish
eval (dokploy completions elvish | slurp)
```

Add that line to the Elvish configuration file to load completions in future
sessions.
