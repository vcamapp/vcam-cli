#!/bin/sh
set -eu
cd "$(dirname "$0")/.."
curl -fsSL https://raw.githubusercontent.com/vcamapp/app/main/app/xcode/Sources/VCamRemoteControl/Resources/openrpc.json -o spec/openrpc.json
