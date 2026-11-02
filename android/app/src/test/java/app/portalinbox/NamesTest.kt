package app.portalinbox

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The names of the app: the label, the application id and the text of the resources. */
class NamesTest {
    // Each word is built from two parts, so that no copy of it stays in the repository.
    private val forbidden = listOf("pa" + "ger", "page" + "me", "circ" + "ular")

    private fun text(path: String) = File(path).readText().lowercase()

    @Test
    fun theLauncherLabelIsPortalInbox() {
        val strings = File("src/main/res/values/strings.xml").readText()
        assertTrue(strings.contains("""<string name="app_name">Portal Inbox</string>"""))
        assertTrue(File("src/main/AndroidManifest.xml").readText().contains("""android:label="@string/app_name""""))
    }

    @Test
    fun theApplicationIdIsFixed() {
        val gradle = File("build.gradle.kts").readText()
        assertTrue(gradle.contains("""applicationId = "app.portalinbox""""))
        assertTrue(gradle.contains("""namespace = "app.portalinbox""""))
    }

    @Test
    fun noForbiddenNameInTheAppFiles() {
        val files = File("src").walkTopDown().filter { it.isFile && it.extension in setOf("kt", "xml", "pro") }.toList() +
            listOf(File("build.gradle.kts"))
        assertTrue(files.size > 20)
        for (f in files) {
            val t = f.readText().lowercase()
            for (w in forbidden) assertFalse("${f.path} has a forbidden name", t.contains(w))
        }
        assertEquals(0, forbidden.count { text("src/main/res/values/strings.xml").contains(it) })
    }
}
