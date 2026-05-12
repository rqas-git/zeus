import Testing
import ZeusCheckSuite

@Test
func zeusCoreChecks() throws {
    for check in ZeusCoreChecks.all {
        try check.body()
    }
}
