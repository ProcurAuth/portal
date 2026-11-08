package app.portalinbox.testsupport

import app.portalinbox.proto.ServerFrame
import app.portalinbox.proto.ServerFrames
import app.portalinbox.proto.StrictJson
import org.json.JSONObject
import java.io.File

/**
 * The golden files of `crates/inbox-core/tests/fixtures`. They are the contract of the wire format:
 * the Rust crate and these tests read the same files. Gradle runs the tests in `android/app`.
 */
object Fixtures {
    private val root: File = File("../../crates/inbox-core/tests/fixtures").also {
        check(it.isDirectory) { "fixtures not found at ${it.absolutePath}: mount the whole repository" }
    }

    class Golden(val name: String, val text: String) {
        val json: JSONObject get() = StrictJson.parseObject(text)
        val tag: String get() = json.getString("t")
    }

    /** Every `.json` file of a folder under `frames/`, sorted by name. */
    fun frames(sub: String): List<Golden> {
        val dir = File(root, "frames/$sub")
        check(dir.isDirectory) { "missing ${dir.absolutePath}" }
        return dir.listFiles { f -> f.isFile && f.extension == "json" }!!.sortedBy { it.name }
            .map { Golden(it.name, it.readText()) }
    }

    fun golden(sub: String, name: String): Golden = frames(sub).first { it.name == name }

    fun server(name: String): ServerFrame = ServerFrames.decode(golden("server", name).text)

    fun text(relative: String): String = File(root, relative).readText()
}
