' Silent launcher for the desktop shortcut.
'
' bluee's binary serves both the CLI and the desktop window, so it is a console
' program. Launching it directly would flash a black console window behind the
' app and leave it sitting in the taskbar. This starts it hidden instead.
'
' For terminal use, run bluee.cmd - you want the console there.

Dim shell, here
Set shell = CreateObject("WScript.Shell")
here = Left(WScript.ScriptFullName, InStrRev(WScript.ScriptFullName, "\"))

shell.CurrentDirectory = here
shell.Run """" & here & "bluee.cmd"" app", 0, False
