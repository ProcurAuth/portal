package app.portalinbox.proto

import org.json.JSONArray
import org.json.JSONObject
import java.math.BigDecimal
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction

/** A frame or a value that breaks a rule of protocol 5. The text names the rule, never a value of the frame. */
class FrameException(message: String) : Exception(message)

/**
 * A strict JSON reader and writer for the frames of protocol 5.
 *
 * The reader refuses a duplicate member name, a trailing text, a number with a leading zero,
 * a control character in a string and a lone surrogate. The Android library `org.json` accepts
 * a duplicate name (the last wins), so the app does not rely on it for the syntax. The reader
 * builds `org.json` objects, so the rest of the code reads them with one API.
 *
 * Number rules of the tree: an integer that fits a `Long` is a `Long`. The text `-0` is the
 * `Double` -0.0. Every other number is a `BigDecimal` that keeps the digits of the text.
 */
object StrictJson {
    private const val MAX_DEPTH = 128

    /** Decodes UTF-8 and refuses a byte sequence that is not valid. */
    fun decodeUtf8(bytes: ByteArray): String = try {
        Charsets.UTF_8.newDecoder()
            .onMalformedInput(CodingErrorAction.REPORT)
            .onUnmappableCharacter(CodingErrorAction.REPORT)
            .decode(ByteBuffer.wrap(bytes)).toString()
    } catch (e: java.nio.charset.CharacterCodingException) {
        throw FrameException("frame is not UTF-8")
    }

    /** Reads a text that holds one JSON object. */
    fun parseObject(text: String): JSONObject =
        parse(text) as? JSONObject ?: throw FrameException("frame is not an object")

    /** Reads a text that holds one JSON value. */
    fun parse(text: String): Any = try {
        Parser(text).parseDocument()
    } catch (e: org.json.JSONException) {
        // The Android library refuses a number that has no finite double value, for example 1e999.
        throw FrameException("not valid JSON: number")
    }

    private class Parser(private val s: String) {
        private var i = 0

        fun parseDocument(): Any {
            skipSpace()
            val v = value(0)
            skipSpace()
            if (i != s.length) fail("trailing text")
            return v
        }

        private fun fail(what: String): Nothing = throw FrameException("not valid JSON: $what")

        private fun skipSpace() {
            while (i < s.length && (s[i] == ' ' || s[i] == '\t' || s[i] == '\n' || s[i] == '\r')) i++
        }

        private fun value(depth: Int): Any {
            if (depth > MAX_DEPTH) fail("nesting")
            if (i >= s.length) fail("end of text")
            return when (val c = s[i]) {
                '{' -> obj(depth)
                '[' -> arr(depth)
                '"' -> string()
                't' -> literal("true", true)
                'f' -> literal("false", false)
                'n' -> literal("null", JSONObject.NULL)
                else -> if (c == '-' || c in '0'..'9') number() else fail("unexpected character")
            }
        }

        private fun literal(word: String, v: Any): Any {
            if (!s.startsWith(word, i)) fail("literal")
            i += word.length
            return v
        }

        private fun obj(depth: Int): JSONObject {
            i++
            val o = JSONObject()
            skipSpace()
            if (peek() == '}') { i++; return o }
            while (true) {
                skipSpace()
                if (peek() != '"') fail("member name")
                val key = string()
                if (o.has(key)) throw FrameException("duplicate member name")
                skipSpace()
                if (peek() != ':') fail("colon")
                i++
                skipSpace()
                o.put(key, value(depth + 1))
                skipSpace()
                when (peek()) {
                    ',' -> i++
                    '}' -> { i++; return o }
                    else -> fail("object")
                }
            }
        }

        private fun arr(depth: Int): JSONArray {
            i++
            val a = JSONArray()
            skipSpace()
            if (peek() == ']') { i++; return a }
            while (true) {
                skipSpace()
                a.put(value(depth + 1))
                skipSpace()
                when (peek()) {
                    ',' -> i++
                    ']' -> { i++; return a }
                    else -> fail("array")
                }
            }
        }

        private fun peek(): Char = if (i < s.length) s[i] else fail("end of text")

        private fun string(): String {
            i++
            val b = StringBuilder()
            while (true) {
                if (i >= s.length) fail("string")
                val c = s[i++]
                when {
                    c == '"' -> return b.toString()
                    c < ' ' -> fail("control character in a string")
                    c == '\\' -> {
                        if (i >= s.length) fail("escape")
                        when (val e = s[i++]) {
                            '"' -> b.append('"')
                            '\\' -> b.append('\\')
                            '/' -> b.append('/')
                            'b' -> b.append('\b')
                            'f' -> b.append('\u000c')
                            'n' -> b.append('\n')
                            'r' -> b.append('\r')
                            't' -> b.append('\t')
                            'u' -> b.append(unit())
                            else -> fail("escape $e")
                        }
                    }
                    Character.isHighSurrogate(c) -> {
                        if (i < s.length && Character.isLowSurrogate(s[i])) b.append(c).append(s[i++]) else fail("surrogate")
                    }
                    Character.isLowSurrogate(c) -> fail("surrogate")
                    else -> b.append(c)
                }
            }
        }

        private fun hex4(): Int {
            if (i + 4 > s.length) fail("escape")
            var v = 0
            repeat(4) {
                val d = Character.digit(s[i++], 16)
                if (d < 0) fail("escape")
                v = v * 16 + d
            }
            return v
        }

        /** After `\u`: one character, or a pair of surrogates written as two escapes. */
        private fun unit(): String {
            val u = hex4()
            val c = u.toChar()
            return when {
                Character.isHighSurrogate(c) -> {
                    if (i + 2 <= s.length && s[i] == '\\' && s[i + 1] == 'u') {
                        i += 2
                        val low = hex4().toChar()
                        if (!Character.isLowSurrogate(low)) fail("surrogate")
                        "$c$low"
                    } else fail("surrogate")
                }
                Character.isLowSurrogate(c) -> fail("surrogate")
                else -> c.toString()
            }
        }

        private fun number(): Any {
            val start = i
            if (s[i] == '-') i++
            if (i >= s.length) fail("number")
            if (s[i] == '0') i++ else if (s[i] in '1'..'9') { while (i < s.length && s[i] in '0'..'9') i++ } else fail("number")
            var integral = true
            if (i < s.length && s[i] == '.') {
                integral = false
                i++
                val d = i
                while (i < s.length && s[i] in '0'..'9') i++
                if (i == d) fail("number")
            }
            if (i < s.length && (s[i] == 'e' || s[i] == 'E')) {
                integral = false
                i++
                if (i < s.length && (s[i] == '+' || s[i] == '-')) i++
                val d = i
                while (i < s.length && s[i] in '0'..'9') i++
                if (i == d) fail("number")
            }
            val token = s.substring(start, i)
            if (integral) {
                if (token == "-0") return -0.0
                token.toLongOrNull()?.let { return it }
            }
            return BigDecimal(token)
        }
    }

    // ---------------------------------------------------------------- writer

    /** Writes a value as compact JSON text: no white space, short escapes, no escape for `/`. */
    fun compact(value: Any?): String = StringBuilder().also { write(value, it) }.toString()

    private fun write(v: Any?, out: StringBuilder) {
        when (v) {
            null, JSONObject.NULL -> out.append("null")
            is Boolean -> out.append(v.toString())
            is Int, is Long -> out.append(v.toString())
            is Double -> if (v.isNaN() || v.isInfinite()) throw FrameException("number") else out.append(v.toString())
            is BigDecimal -> out.append(v.toString())
            is String -> writeString(v, out)
            is JSONObject -> {
                out.append('{')
                var first = true
                for (k in v.keys().asSequence().toList().sorted()) {
                    if (!first) out.append(',')
                    first = false
                    writeString(k, out)
                    out.append(':')
                    write(v.get(k), out)
                }
                out.append('}')
            }
            is JSONArray -> {
                out.append('[')
                for (n in 0 until v.length()) {
                    if (n > 0) out.append(',')
                    write(v.get(n), out)
                }
                out.append(']')
            }
            else -> throw FrameException("value of an unknown type")
        }
    }

    private fun writeString(s: String, out: StringBuilder) {
        out.append('"')
        for (c in s) {
            when {
                c == '"' -> out.append("\\\"")
                c == '\\' -> out.append("\\\\")
                c == '\b' -> out.append("\\b")
                c == '\t' -> out.append("\\t")
                c == '\n' -> out.append("\\n")
                c == '\u000c' -> out.append("\\f")
                c == '\r' -> out.append("\\r")
                c < ' ' -> out.append("\\u").append(c.code.toString(16).padStart(4, '0'))
                else -> out.append(c)
            }
        }
        out.append('"')
    }
}
