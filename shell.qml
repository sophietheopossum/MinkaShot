import Quickshell
import Quickshell.Io
import QtQuick
import "services"
import "modules"

// MinkaShot — the Minka freeze-frame screenshot tool. Print (compositor
// keybind) broadcasts ui.minkashot over the ShojiWM socket to arm the
// interactive overlay; the IPC target below covers scripted captures.
//
//   qs -p <dir> ipc call shot interactive
//   qs -p <dir> ipc call shot full <screen|""> <path|"">
//   qs -p <dir> ipc call shot region <screen|""> "x,y,w,h" <path|"">
//   qs -p <dir> ipc call shot win <window title> <path|"">
//   qs -p <dir> ipc call shot cancel
ShellRoot {
    Variants {
        model: Quickshell.screens

        Scope {
            id: scope

            required property var modelData

            CaptureOverlay {
                modelData: scope.modelData
            }

            CountdownChip {
                modelData: scope.modelData
            }
        }
    }

    IpcHandler {
        target: "shot"

        function interactive(): void {
            ShotState.interactive();
        }

        function cancel(): void {
            ShotState.disarm();
        }

        function full(screen: string, path: string): void {
            ShotState.fullJob(screen, path);
        }

        function region(screen: string, spec: string, path: string): void {
            ShotState.regionJob(screen, spec, path);
        }

        function win(title: string, path: string): void {
            ShotState.windowJob(title, path);
        }
    }
}