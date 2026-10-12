Type "pgpu run --class render --as blender -- ./render.sh &"
Enter
Sleep 600ms
Type "pgpu run --class test --as ci -- ./tests.sh &"
Enter
Sleep 600ms
Type "pgpu run --class bench --as bench -- ./bench.sh &"
Enter
Sleep 900ms
Type "pgpu run --class search --as search -- ./search.sh &"
Enter
Sleep 900ms
Type "clear; pgpu status"
Enter
Sleep 4s
Type "clear; pgpu status"
Enter
Sleep 5s
