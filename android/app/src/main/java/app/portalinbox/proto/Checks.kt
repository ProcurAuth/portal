package app.portalinbox.proto

import org.json.JSONArray
import org.json.JSONObject

/** Largest plain text of one frame (one Noise message). The same value as `MAX_PLAINTEXT` of `inbox-core`. */
const val MAX_PLAINTEXT = 65519

const val MAX_TITLE_CHARS = 128
const val MAX_EXPLANATION_CHARS = 8000
const val MAX_COMMAND_BYTES = 32 * 1024
const val MAX_COMMAND_LABEL_CHARS = 32
const val MAX_USER_TEXT_CHARS = 8000
const val MIN_OPTIONS = 2
const val MAX_OPTIONS = 8
const val MAX_OPTION_LABEL_CHARS = 48
const val MAX_OPTION_DESCRIPTION_CHARS = 160
const val MAX_PLACEHOLDER_CHARS = 128
const val MIN_FORM_QUESTIONS = 1
const val MAX_FORM_QUESTIONS = 4
const val MAX_FORM_HEADER_CHARS = 16
const val MAX_FORM_TEXT_CHARS = 600
const val MAX_SYNC_ENTRIES = 500
const val MAX_DIGEST_IDS = 200
const val MAX_SYNC_AGENTS = 200
const val MAX_AGENT_NAME_CHARS = 32
const val MAX_AGENT_LABEL_CHARS = 64
const val MAX_AGENT_CLIENT_CHARS = 64
const val MAX_AGENT_USER_AGENT_CHARS = 128
const val MAX_AGENT_NET_CHARS = 43
const val MAX_AGENT_NETS_SEEN = 8
const val MAX_VIEW_FIELDS = 12
const val MAX_VIEW_TITLE_CHARS = 128
const val MAX_VIEW_LABEL_CHARS = 48
const val MAX_POINTER_BYTES = 256

/** Largest absolute value of an integer in `op`: 2^53 - 1. */
const val MAX_SAFE_INTEGER = 9_007_199_254_740_991L

/** The text rules of protocol 5. They are the rules of `crates/inbox-core/src/proto.rs`. */
internal object Check {
    fun bad(what: String): Nothing = throw FrameException("invalid frame: $what")

    private fun chars(text: String): Int = text.codePointCount(0, text.length)

    private fun hasControl(text: String): Boolean {
        var i = 0
        while (i < text.length) {
            val cp = text.codePointAt(i)
            if (Character.getType(cp) == Character.CONTROL.toInt()) return true
            i += Character.charCount(cp)
        }
        return false
    }

    fun utf8Size(text: String): Int = text.toByteArray(Charsets.UTF_8).size

    /** One line of 1 to `max` characters with no control character. */
    fun line(text: String, max: Int, what: String): String {
        val n = chars(text)
        if (n == 0 || n > max || hasControl(text)) bad(what)
        return text
    }

    private fun isBidi(cp: Int) = cp in 0x202A..0x202E || cp in 0x2066..0x2069

    /** The title of an item: a line with no bidi control and no line separator. */
    fun title(text: String, what: String): String {
        line(text, MAX_TITLE_CHARS, what)
        var i = 0
        while (i < text.length) {
            val cp = text.codePointAt(i)
            if (isBidi(cp) || cp == 0x2028 || cp == 0x2029) bad(what)
            i += Character.charCount(cp)
        }
        return text
    }

    /**
     * Whether a character is of the class Cf (format) of Unicode: the bidirectional controls, the zero-width characters,
     * the invisible operators and the tag characters. It is the list of `is_format_char` of the core crate.
     */
    fun isFormatChar(cp: Int): Boolean = cp == 0x00AD || cp in 0x0600..0x0605 || cp == 0x061C || cp == 0x06DD ||
        cp == 0x070F || cp in 0x0890..0x0891 || cp == 0x08E2 || cp == 0x180E || cp in 0x200B..0x200F ||
        cp in 0x202A..0x202E || cp in 0x2060..0x2064 || cp in 0x2066..0x206F || cp == 0xFEFF ||
        cp in 0xFFF9..0xFFFB || cp == 0x110BD || cp == 0x110CD || cp in 0x13430..0x1343F ||
        cp in 0x1BCA0..0x1BCA3 || cp in 0x1D173..0x1D17A || cp == 0xE0001 || cp in 0xE0020..0xE007F

    /**
     * A text that an agent or a client sent: one line of 1 to `max` characters, with no control character, no format
     * character and no line or paragraph separator.
     */
    fun untrustedLine(text: String, max: Int, what: String): String {
        line(text, max, what)
        var i = 0
        while (i < text.length) {
            val cp = text.codePointAt(i)
            if (isFormatChar(cp) || cp == 0x2028 || cp == 0x2029) bad(what)
            i += Character.charCount(cp)
        }
        return text
    }

    /** The name that the owner gives to an agent: 1 to 32 characters with no NUL. The server checks the other rules. */
    fun agentName(text: String, what: String): String {
        text(text, MAX_AGENT_NAME_CHARS, what)
        if (text.contains('\u0000')) bad(what)
        return text
    }

    /** A free text of 1 to `max` characters. Control characters stay: the screen shows them. */
    fun text(text: String, max: Int, what: String): String {
        val n = chars(text)
        if (n == 0 || n > max) bad(what)
        return text
    }

    fun isId(text: String): Boolean =
        text.length == 32 && text.all { it in '0'..'9' || it in 'a'..'f' }

    fun id(text: String, what: String): String {
        if (!isId(text)) bad(what)
        return text
    }

    fun time(ms: Long, what: String): Long {
        if (ms < 0) bad(what)
        return ms
    }

    /** 1 to 64 characters of `A-Z a-z 0-9 _ -`. */
    fun clientMsgId(text: String): String {
        val ok = text.length in 1..64 && text.all { (it in 'a'..'z') || (it in 'A'..'Z') || (it in '0'..'9') || it == '_' || it == '-' }
        if (!ok) bad("client_msg_id")
        return text
    }

    /** A token of the vocabulary of the server: 1 to 32 characters of `a-z 0-9 _`. */
    fun token(text: String, what: String): String {
        val ok = text.length in 1..32 && text.all { (it in 'a'..'z') || (it in '0'..'9') || it == '_' }
        if (!ok) bad(what)
        return text
    }

    /** `sha256:` and 64 lowercase hexadecimal digits. */
    fun isEffect(text: String): Boolean =
        text.startsWith("sha256:") && text.length == 71 && text.substring(7).all { it in '0'..'9' || it in 'a'..'f' }

    fun code(text: String, what: String): String {
        val ok = text.length in 4..8 && text.all { (it in 'A'..'Z') || (it in '0'..'9') }
        if (!ok) bad(what)
        return text
    }

    fun hhmm(text: String, what: String): String {
        val ok = text.length == 5 && text[2] == ':' &&
            (text.substring(0, 2) + text.substring(3)).all { it in '0'..'9' } &&
            text.substring(0, 2).toInt() < 24 && text.substring(3).toInt() < 60
        if (!ok) bad(what)
        return text
    }

    fun tz(text: String, what: String): String {
        val ok = text.length in 1..64 &&
            text.all { (it in 'a'..'z') || (it in 'A'..'Z') || (it in '0'..'9') || it == '_' || it == '+' || it == '-' || it == '/' }
        if (!ok) bad(what)
        return text
    }

    /** An option id: 1 to 32 characters of `a-z 0-9 _ -`. */
    fun optionId(text: String): Boolean =
        text.length in 1..32 && text.all { (it in 'a'..'z') || (it in '0'..'9') || it == '_' || it == '-' }

    /** The rules of a list of options with `multiple`, `min` and `max`. */
    fun choice(multiple: Boolean, min: Int, max: Int, options: List<InputOption>, what: String) {
        val n = options.size
        if (n !in MIN_OPTIONS..MAX_OPTIONS) bad(what)
        if (min < 1 || min > max || max > n) bad(what)
        if (!multiple && (min != 1 || max != 1)) bad(what)
        val seen = HashSet<String>()
        for (o in options) {
            if (!optionId(o.id) || !seen.add(o.id)) bad(what)
            line(o.label, MAX_OPTION_LABEL_CHARS, what)
            o.description?.let { line(it, MAX_OPTION_DESCRIPTION_CHARS, what) }
        }
    }

    /** The rules of `op`: objects, arrays, strings, booleans, null and integers of at most 2^53-1. */
    fun op(value: JSONObject): JSONObject {
        checkOp(value, 0)
        return value
    }

    private fun checkOp(v: Any?, depth: Int) {
        if (depth > 64) bad("approval.op")
        when (v) {
            JSONObject.NULL, is Boolean, is String -> {}
            is Long -> if (v < -MAX_SAFE_INTEGER || v > MAX_SAFE_INTEGER) bad("approval.op")
            is Int -> {}
            is JSONArray -> for (n in 0 until v.length()) checkOp(v.get(n), depth + 1)
            is JSONObject -> for (k in v.keys()) checkOp(v.get(k), depth + 1)
            else -> bad("approval.op")
        }
    }
}

/**
 * A reader of one JSON object that refuses what the rules refuse: a member of the wrong type, a
 * missing member and an unknown member. A member with the value `null` counts as absent.
 * Call [done] when every member is read: it refuses each member that no call asked for.
 */
internal class Obj(private val o: JSONObject, private val where: String) {
    private val used = HashSet<String>()

    private fun raw(key: String): Any? {
        used.add(key)
        return if (!o.has(key) || o.isNull(key)) null else o.get(key)
    }

    private fun bad(key: String): Nothing = Check.bad("$where.$key")

    fun has(key: String): Boolean = raw(key) != null

    fun str(key: String): String = raw(key) as? String ?: bad(key)

    fun optStr(key: String): String? {
        val v = raw(key) ?: return null
        return v as? String ?: bad(key)
    }

    private fun integer(v: Any?): Long? = when (v) {
        is Long -> v
        is Int -> v.toLong()
        else -> null
    }

    fun long(key: String): Long = integer(raw(key)) ?: bad(key)

    fun optLong(key: String): Long? {
        val v = raw(key) ?: return null
        return integer(v) ?: bad(key)
    }

    /** A time or a count that is 0 or more. */
    fun unsigned(key: String): Long = long(key).also { if (it < 0) bad(key) }

    fun optUnsigned(key: String): Long? = optLong(key)?.also { if (it < 0) bad(key) }

    fun bool(key: String): Boolean = raw(key) as? Boolean ?: bad(key)

    fun optBool(key: String, default: Boolean): Boolean {
        val v = raw(key) ?: return default
        return v as? Boolean ?: bad(key)
    }

    /** A number that can have a fraction. */
    fun optDouble(key: String): Double? {
        val v = raw(key) ?: return null
        return (v as? Number)?.toDouble() ?: bad(key)
    }

    fun obj(key: String): Obj = Obj(raw(key) as? JSONObject ?: bad(key), "$where.$key")

    fun optObj(key: String): Obj? {
        val v = raw(key) ?: return null
        return Obj(v as? JSONObject ?: bad(key), "$where.$key")
    }

    fun rawObj(key: String): JSONObject = raw(key) as? JSONObject ?: bad(key)

    fun arr(key: String): JSONArray = raw(key) as? JSONArray ?: bad(key)

    fun optArr(key: String): JSONArray? {
        val v = raw(key) ?: return null
        return v as? JSONArray ?: bad(key)
    }

    fun <E> enum(key: String, values: Array<E>): E where E : Enum<E>, E : Wire =
        optEnum(key, values) ?: bad(key)

    fun <E> optEnum(key: String, values: Array<E>): E? where E : Enum<E>, E : Wire {
        val v = raw(key) ?: return null
        val s = v as? String ?: bad(key)
        return values.firstOrNull { it.wire == s } ?: bad(key)
    }

    /** Refuses each member that no accessor read. */
    fun done() {
        for (k in o.keys()) if (k !in used) Check.bad("$where: unknown member")
    }
}

/** An enumeration with a name on the wire. */
interface Wire {
    val wire: String
}

internal fun JSONArray.strings(what: String): List<String> = (0 until length()).map { get(it) as? String ?: Check.bad(what) }

internal fun JSONArray.objects(what: String): List<JSONObject> = (0 until length()).map { get(it) as? JSONObject ?: Check.bad(what) }
