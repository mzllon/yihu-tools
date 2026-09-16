#!/usr/bin/env bash
# Compatibility entry point. Never installs from checkout paths.
set -euo pipefail
printf '%s\n' 'Build: ./scripts/build-package.sh' \
  'Extract dist/yihu-<version>-linux-<arch>.tar.gz, then run inside it:' \
  '  ./reinstall.sh --dry-run' '  ./reinstall.sh'
