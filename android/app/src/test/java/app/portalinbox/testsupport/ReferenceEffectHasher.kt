package app.portalinbox.testsupport

import app.portalinbox.core.EffectHasher
import app.portalinbox.core.EffectResult
import app.portalinbox.core.ResolvedField
import app.portalinbox.core.ResolvedView
import app.portalinbox.core.ViewResult
import app.portalinbox.proto.Check
import app.portalinbox.proto.FrameException
import app.portalinbox.proto.Obj
import app.portalinbox.proto.StrictJson
import app.portalinbox.proto.ViewSpec
import org.json.JSONArray
import org.json.JSONObject
import java.security.MessageDigest

/**
 * An independent Kotlin version of the effect rule (ProcurAuth 17.3), for the JVM tests only. The
 * real hash runs in Rust through JNI, and the tests of `inbox-core` prove it. This fake lets the
 * tests check the golden approval and the vectors without a native library. The test of the
 * vectors proves that the fake gives the same hashes as the Rust code.
 */
class ReferenceEffectHasher : EffectHasher {
    var effectCalls = 0
        private set
    var viewCalls = 0
        private set

    fun canonical(service: String, action: String, resource: String, op: JSONObject): String =
        "{\"action\":${str(action)},\"op\":${jcs(op)},\"resource\":${str(resource)},\"service\":${str(service)},\"v\":1}"

    override fun effect(service: String, action: String, resource: String, opJson: String): EffectResult {
        effectCalls++
        val op = try {
            Check.op(StrictJson.parse(opJson) as? JSONObject ?: return EffectResult.Failure("invalid_op", "op is not an object"))
        } catch (e: FrameException) {
            return EffectResult.Failure("invalid_op", e.message ?: "")
        }
        val digest = MessageDigest.getInstance("SHA-256").digest(canonical(service, action, resource, op).toByteArray(Charsets.UTF_8))
        return EffectResult.Hash("sha256:" + digest.joinToString("") { "%02x".format(it) })
    }

    override fun view(opJson: String, viewJson: String): ViewResult {
        viewCalls++
        val op = try {
            Check.op(StrictJson.parse(opJson) as? JSONObject ?: return ViewResult.Failure("invalid_op", "op is not an object"))
        } catch (e: FrameException) {
            return ViewResult.Failure("invalid_op", e.message ?: "")
        }
        val spec = try {
            ViewSpec.read(Obj(StrictJson.parseObject(viewJson), "view"))
        } catch (e: FrameException) {
            return ViewResult.Failure("invalid_view", e.message ?: "")
        }
        val fields = spec.fields.map { (label, ptr) ->
            val v = pointer(op, ptr) ?: return ViewResult.Failure("invalid_view", "a pointer names no value of op")
            val text = when (v) {
                is String -> v
                is JSONObject, is JSONArray -> return ViewResult.Failure("invalid_view", "a field names an object or an array")
                else -> StrictJson.compact(v)
            }
            ResolvedField(label, text)
        }
        val body = pointer(op, spec.body) as? String ?: return ViewResult.Failure("invalid_view", "the body does not name a string")
        return ViewResult.Resolved(ResolvedView(spec.title, fields, body, spec.bodyLabel))
    }

    private fun pointer(op: Any, ptr: String): Any? {
        if (ptr.isEmpty()) return op
        if (!ptr.startsWith("/")) return null
        var cur: Any = op
        for (raw in ptr.substring(1).split("/")) {
            val token = raw.replace("~1", "/").replace("~0", "~")
            cur = when (cur) {
                is JSONObject -> if (cur.has(token)) cur.get(token) else return null
                is JSONArray -> {
                    val ok = token.isNotEmpty() && token.all { it in '0'..'9' } && (token == "0" || !token.startsWith("0"))
                    if (!ok || token.toInt() >= cur.length()) return null
                    cur.get(token.toInt())
                }
                else -> return null
            }
        }
        return cur
    }

    private fun jcs(v: Any?): String = when (v) {
        JSONObject.NULL, null -> "null"
        is Boolean, is Long, is Int -> v.toString()
        is String -> str(v)
        is JSONArray -> (0 until v.length()).joinToString(",", "[", "]") { jcs(v.get(it)) }
        is JSONObject -> v.keys().asSequence().toList().sorted().joinToString(",", "{", "}") { str(it) + ":" + jcs(v.get(it)) }
        else -> throw IllegalArgumentException("not in the subset")
    }

    private fun str(s: String): String {
        val b = StringBuilder("\"")
        for (c in s) {
            when {
                c == '"' -> b.append("\\\"")
                c == '\\' -> b.append("\\\\")
                c == '\b' -> b.append("\\b")
                c == '\t' -> b.append("\\t")
                c == '\n' -> b.append("\\n")
                c == '\u000c' -> b.append("\\f")
                c == '\r' -> b.append("\\r")
                c < ' ' -> b.append("\\u%04x".format(c.code))
                else -> b.append(c)
            }
        }
        return b.append('"').toString()
    }
}

/** A hasher whose answers the test sets. It records the arguments of each call. */
class ScriptedEffectHasher(
    var effectResult: (String, String, String, String) -> EffectResult,
    var viewResult: (String, String) -> ViewResult,
) : EffectHasher {
    val effectCalls = ArrayList<List<String>>()
    val viewCalls = ArrayList<List<String>>()

    override fun effect(service: String, action: String, resource: String, opJson: String): EffectResult {
        effectCalls += listOf(service, action, resource, opJson)
        return effectResult(service, action, resource, opJson)
    }

    override fun view(opJson: String, viewJson: String): ViewResult {
        viewCalls += listOf(opJson, viewJson)
        return viewResult(opJson, viewJson)
    }
}
