Type "pgpu run --as sim --memory 64M -- ./hungry.sh; echo $?"
Enter
Sleep 9s
Type "pgpu run --as sim --budget 2s -- ./forever.sh &"
Enter
Sleep 3s
Type "pgpu status"
Enter
Sleep 2s
Type "wait $!; echo $?"
Enter
Sleep 3s
