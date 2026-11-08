# VCam CLI

Control [VCam](https://vcamapp.com/en/) from your terminal, scripts, and AI agents through the VCam API.

> [!NOTE]
> The VCam API is currently available only in [VCamNova](https://vcamapp.com/en/vcamnova), the next version of VCam in development. The released VCam apps do not offer it yet.

## What it does

- **Calls every API method by its name.** `vcam scene.load --sceneId 3` works for any method the running VCam offers, including ones added after this tool was built.
- **Checks your input before sending.** Values are read by the types in the API, so `--text 123` stays text, and missing or misspelled parameters are reported with the right names.
- **Uploads files in one step.** `vcam image.import photo.png` sends the file and adds it to the scene.
- **Saves the output.** `vcam frame.capture -o frame.png` writes what VCam is showing to a file.
- **Follows events.** `vcam events motion.stopped --count 1` waits until a motion ends.
- **Lets AI agents run VCam.** `vcam mcp` is an MCP server, so Claude and other MCP clients can build scenes, place text and images, play motions, and look at the result.

## Requirements

- [VCamNova](https://vcamapp.com/en/vcamnova) with the VCam API turned on in Settings > Integration
- To control VCam on another computer, allow connections from other devices there and set `VCAM_TOKEN` to the token shown in the same settings

## Install

macOS and Linux with [Homebrew](https://brew.sh/):

```sh
brew install vcamapp/tap/vcam-cli
```

macOS and Linux without Homebrew:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/vcamapp/vcam-cli/releases/latest/download/vcam-installer.sh | sh
```

Windows:

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/vcamapp/vcam-cli/releases/latest/download/vcam-installer.ps1 | iex"
```

Each installs the `vcam` command. The archives are also on the [releases page](https://github.com/vcamapp/vcam-cli/releases).

## Usage

```sh
vcam methods                      # lists the methods
vcam text.add --help              # shows the parameters of a method
vcam scene.list
vcam text.add --text "Hello" --placement '{"x":0.3,"y":0.35,"width":0.25}'
vcam expression.set --params '{"expressions":[{"name":"happy","weight":0.8}]}'
vcam avatar.import avatar.vrm --load
vcam frame.capture --maxWidth 1280 -o frame.jpg
vcam events motion.started motion.stopped
```

- Parameters are passed as `--name value` with the names used by the API. Objects and arrays are written as JSON, and `--params` takes all parameters as one JSON object (`--params -` reads it from standard input).
- Results are printed as JSON. Add `--json` to always print one line, errors included.
- `--host` and `--port` (or `VCAM_HOST` and `VCAM_PORT`) choose the VCam to control. The default is this computer on port 34965.

| Exit code | Meaning |
| --- | --- |
| 0 | Success |
| 1 | VCam returned an error |
| 2 | The command was wrong, so nothing was sent |
| 3 | VCam could not be reached or the connection was lost |
| 4 | `vcam events` timed out before `--count` events arrived |

## Use with AI agents (MCP)

Add VCam CLI as an MCP server in your client. For Claude Code:

```sh
claude mcp add vcam -- vcam mcp
```

For clients configured with JSON:

```json
{
  "mcpServers": {
    "vcam": { "command": "vcam", "args": ["mcp"] }
  }
}
```

Each API method becomes a tool. Uploads take a local file path, `frame_capture` returns the image, and `events_wait` returns what happened in VCam since the last call. `vcam mcp --tools scene,object,text` offers only the tools in those groups, which keeps the tool list short.

## Development

```sh
cargo test
```

`spec/openrpc.json` is a copy of the VCam API specification. It answers `--help` while VCam is not running and keeps the tests in step with the API. Refresh it with `scripts/update-spec.sh`. See [CONTRIBUTING.md](CONTRIBUTING.md) before opening a pull request.

## License

MIT
