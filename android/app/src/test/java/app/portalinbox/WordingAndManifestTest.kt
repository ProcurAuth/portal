package app.portalinbox

import java.io.File
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/** The word "Portal" in the strings of the app (lot MP2), and the exports of the enrollment screens in the manifest. */
class WordingAndManifestTest {
    private val files = listOf("src/main/res/values/strings.xml", "src/main/res/values/strings_portals.xml", "src/main/res/values/strings_agents.xml")
    private val entry = Regex("""<string name="([^"]+)"[^>]*>([^<]*)</string>|<item quantity="[a-z]+">([^<]*)</item>""")

    private fun values(): List<Pair<String, String>> = files.flatMap { path ->
        entry.findAll(File(path).readText()).map { (it.groupValues[1].ifEmpty { "plural" }) to (it.groupValues[2] + it.groupValues[3]) }.toList()
    }

    @Test
    fun noTextOfTheAppSaysServer() {
        val bad = values().filter { it.second.contains("server", ignoreCase = true) }.map { it.first }
        assertTrue("the word is Portal, not Server: $bad", bad.isEmpty())
    }

    @Test
    fun stringNamesAreUniqueAcrossTheFiles() {
        val names = files.flatMap { Regex("""<string name="([^"]+)"""").findAll(File(it).readText()).map { m -> m.groupValues[1] }.toList() }
        assertEquals(names.size, names.toSet().size)
    }

    @Test
    fun theTextsOfTheAgentsAreAsciiOnly() {
        // Lot G4: no hidden, bidirectional or look-alike character can enter the screen through a resource.
        val text = File("src/main/res/values/strings_agents.xml").readText()
        val bad = text.filter { it.code !in 0x20..0x7E && it != '\n' }
        assertEquals("only ASCII: <${bad.map { "U+%04X".format(it.code) }}>", "", bad)
    }

    @Test
    fun noStringStartsOrEndsWithASpace() {
        // Android removes these spaces: the text would not be what the file shows.
        val bad = values().filter { it.second != it.second.trim() }.map { it.first }
        assertTrue("$bad", bad.isEmpty())
    }

    /** The text of the element `<activity>` with this name: up to its end tag, or up to the end of its start tag when it is self-closing. */
    private fun activity(name: String): String {
        val manifest = File("src/main/AndroidManifest.xml").readText()
        val at = manifest.indexOf("android:name=\"$name\"")
        assertTrue("$name is in the manifest", at >= 0)
        val start = manifest.lastIndexOf("<activity", at)
        val openEnd = manifest.indexOf('>', at)
        val end = if (manifest[openEnd - 1] == '/') openEnd else manifest.indexOf("</activity>", openEnd)
        return manifest.substring(start, end)
    }

    @Test
    fun onlyTheLinkScreenIsExportedAndReenrollIsNot() {
        assertTrue(activity(".ui.AddServerActivity").contains("""android:exported="true""""))
        assertTrue(activity(".ui.AddServerActivity").contains("BROWSABLE"))
        val reenroll = activity(".ui.ReenrollActivity")
        assertTrue(reenroll.contains("""android:exported="false""""))
        assertFalse(reenroll.contains("intent-filter"))
        assertTrue(activity(".ui.PortalActivity").contains("""android:exported="false""""))
        // Lot G4: the screens of the agents act on an agent of the owner. Only the app opens them.
        val agent = activity(".ui.AgentActivity")
        assertTrue(agent.contains("""android:exported="false""""))
        assertFalse(agent.contains("intent-filter"))
    }

    @Test
    fun aLinkCannotReachTheReenrollMode() {
        // The exported screen never reads an id of a portal to enroll again: only the class that is not exported does.
        val source = File("src/main/java/app/portalinbox/ui/AddServerActivity.kt").readText()
        val base = source.substringBefore("class ReenrollActivity")
        assertFalse(base.contains("EXTRA_PORTAL"))
        assertTrue(base.contains("protected open fun reenrollId(): Long? = null"))
    }
}
