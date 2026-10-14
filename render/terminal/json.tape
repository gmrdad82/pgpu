Type "pgpu run --class render --as blender -- ./long.sh &"
Enter
Sleep 300ms
Type "pgpu run --class test --as ci -- ./tests.sh &"
Enter
Sleep 300ms
Type "pgpu run --class search --as search -- ./search.sh &"
Enter
Sleep 800ms
Type "clear; pgpu status --json | jq -c '{gpu_busy, vram}'"
Enter
Sleep 1500ms
Type "pgpu status --json | jq -c '.jobs[] | {as, class, state, why}'"
Enter
Sleep 4s
