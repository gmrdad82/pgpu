#!/bin/sh
set -eu
mkdir -p "$1/run" "$1/state" "$1/config/pgpu"
cd "$1"
printf 'GPU_SLOTS=2\n' > config/pgpu/config
for job in render:9 tests:14 bench:6 search:6; do
  printf '#!/bin/sh\nsleep %s\n' "${job#*:}" > "${job%:*}.sh"
  chmod +x "${job%:*}.sh"
done
