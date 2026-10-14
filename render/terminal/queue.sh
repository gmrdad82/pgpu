#!/bin/sh
set -eu
mkdir -p "$1/run" "$1/state" "$1/config/pgpu"
cd "$1"
printf 'GPU_SLOTS=2\n' > config/pgpu/config
for job in render:9 tests:14 bench:6 search:6 forever:60 long:30; do
  printf '#!/bin/sh\nsleep %s\n' "${job#*:}" > "${job%:*}.sh"
  chmod +x "${job%:*}.sh"
done
printf '#!/bin/sh\nfor pass in 1 2 3 4 5 6; do sleep 5; pgpu turn; done\n' > passes.sh
printf '#!/bin/sh\necho "preview: frame drawn"\n' > preview.sh
printf '#!/bin/sh\nexec python3 -c "b = bytearray(300 << 20)"\n' > hungry.sh
chmod +x passes.sh preview.sh hungry.sh
