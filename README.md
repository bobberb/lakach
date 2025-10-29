# Lakach

A terminal user interface (TUI) written using Claude Sonnet 4.5 in rust (ratatui) for browsing and downloading remote folders via SSH+rsync.

## Features

- Browse remote folder structures via SSH
- Fuzzy filter to quickly find folders
- Queue multiple downloads
- Background download processing
- Download history tracking
- Navigate while downloads are running
- Customizable download destination

## Installation

### Using Nix Flakes

```bash
# Pull and run from github
nix run github:bobberb/lakach -- user@remote ./local_folder

# Clone and run locally
nix run . -- user@remote ./local_folder
```

### Using Cargo

```bash
cargo build --release
./target/release/lakach
```

## Usage

```bash
lakach <remote_source> <local_dest>
```

### Examples

Maintains rsync syntax:

```bash
# Browse remote home directory
lakach user@hostname ./downloads

# Browse specific remote path
lakach user@hostname:/path/to/folder ./downloads
```

## Key Bindings

### Browser Tab

| Key | Action |
|-----|--------|
| `j` / `k` or `↑` / `↓` | Navigate up/down |
| `PgUp` / `PgDn` | Jump 10 items |
| `Enter` | Enter selected folder |
| `Backspace` | Go back to parent folder |
| `/` | Filter folders (fuzzy search) |
| `d` | Queue selected folder for download |
| `Shift+T` | Change download destination |
| `Tab` | Switch tabs |
| `q` | Quit |

### Downloads Tab

| Key | Action |
|-----|--------|
| `j` / `k` or `↑` / `↓` | Navigate up/down |
| `PgUp` / `PgDn` | Jump 10 items |
| `Tab` | Switch tabs |
| `q` | Quit |

### History Tab

| Key | Action |
|-----|--------|
| `j` / `k` or `↑` / `↓` | Navigate up/down |
| `PgUp` / `PgDn` | Jump 10 items |
| `x` | Clear selected history item |
| `Shift+X` | Clear all history |
| `Tab` | Switch tabs |
| `q` | Quit |

## How It Works

1. **Browse**: Navigate through remote folders using SSH
2. **Filter**: Press `/` to fuzzy search folder names in real-time
3. **Download**: Press `d` to queue a folder for download using rsync
4. **Monitor**: Switch to the Downloads tab to see progress
5. **History**: View completed downloads in the History tab

Downloads are processed in the background using `rsync -vrtzhP`, allowing you to continue browsing while transfers are in progress.

## Requirements

- `ssh`
- `rsync`
- SSH keys configured for passwordless authentication (recommended)

## License

MIT
