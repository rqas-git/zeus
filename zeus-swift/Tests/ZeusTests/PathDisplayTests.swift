import Testing
import ZeusCore

@Test
func abbreviatesHomePaths() {
    #expect(
        PathDisplay.abbreviatingHome(
            in: "/Users/example/project",
            homeDirectory: "/Users/example"
        ) == "~/project"
    )
}

@Test
func leavesExternalPathsUntouched() {
    #expect(
        PathDisplay.abbreviatingHome(
            in: "/Volumes/work/project",
            homeDirectory: "/Users/example"
        ) == "/Volumes/work/project"
    )
    #expect(
        PathDisplay.abbreviatingHome(
            in: "/Users/example-other/project",
            homeDirectory: "/Users/example"
        ) == "/Users/example-other/project"
    )
}
