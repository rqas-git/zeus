import Testing
import ZeusCore

@Test
func mapsKnownToolMetadata() {
    let metadata = ToolMetadata.forName("git_commit")

    #expect(metadata.name == "git_commit")
    #expect(metadata.action == "committing")
    #expect(metadata.iconName == "arrow.trianglehead.branch")
}

@Test
func summarizesToolTargets() {
    let exec = ToolMetadata.forName("exec_command")
    #expect(exec.target(fromArgumentsJSON: #"{"command":"swift test"}"#) == #""swift test""#)

    let add = ToolMetadata.forName("git_add")
    #expect(add.target(fromArgumentsJSON: #"{"paths":["a.swift","b.swift"]}"#) == "a.swift +1")

    let patch = ToolMetadata.forName("apply_patch")
    #expect(
        patch.target(
            fromArgumentsJSON: #"{"patch":"*** Begin Patch\n*** Update File: Sources/App.swift\n*** End Patch"}"#
        ) == "Sources/App.swift"
    )
}
