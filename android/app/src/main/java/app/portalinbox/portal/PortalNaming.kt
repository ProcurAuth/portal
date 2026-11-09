package app.portalinbox.portal

import app.portalinbox.ui.VisibleText

/**
 * The rules of the nickname of a portal (proposal 2, rule 3; proposal 3.1). Pure Kotlin.
 *
 * - The nickname is required. The phone never proposes the name that the portal gives.
 * - It has 1 to 16 characters (Unicode code points) after the spaces at both ends are removed.
 * - It has no control character, no bidi character, no zero-width or other invisible character, and no space other
 *   than the plain space. These are refused, never removed: what the user sees is what is stored.
 * - It is unique among the portals of the phone. Uniqueness uses [normalize]: `Prod`, `prod`, `Pr0d` and `Pr-od`
 *   are the same name.
 */
object PortalNaming {
    const val MIN_LENGTH = 1
    const val MAX_LENGTH = 16

    enum class Reason {
        /** Nothing, or only plain spaces. */
        EMPTY,
        /** More than 16 characters. */
        TOO_LONG,
        /** A control, bidi, zero-width, invisible or unassigned character, or a space that is not the plain space. */
        HIDDEN_CHARACTERS,
        /** Only spaces and the separators `-`, `_` and `.`: nothing to tell it from another name. */
        ONLY_SEPARATORS,
        /** Another portal has the same name after normalization. */
        TAKEN,
    }

    sealed interface Check {
        /** [nickname] is the text to store (spaces at both ends removed). [key] is the normalized form for the unique column. */
        data class Valid(val nickname: String, val key: String) : Check

        /** [conflict] is the other portal when the reason is [Reason.TAKEN]. */
        data class Invalid(val reason: Reason, val conflict: PortalIdentity? = null) : Check
    }

    /**
     * Checks a nickname. [existing] are the portals of the phone. [ignoreId] is the portal that is being renamed: its own
     * name does not conflict.
     */
    fun validate(raw: String, existing: Collection<PortalIdentity>, ignoreId: Long? = null): Check {
        if (hasHidden(raw)) return Check.Invalid(Reason.HIDDEN_CHARACTERS)
        val nickname = raw.trim(' ')
        if (nickname.isEmpty()) return Check.Invalid(Reason.EMPTY)
        if (nickname.codePointCount(0, nickname.length) > MAX_LENGTH) return Check.Invalid(Reason.TOO_LONG)
        val key = normalize(nickname)
        if (key.isEmpty()) return Check.Invalid(Reason.ONLY_SEPARATORS)
        val other = existing.firstOrNull { it.id != ignoreId && normalize(it.nickname) == key }
        if (other != null) return Check.Invalid(Reason.TAKEN, other)
        return Check.Valid(nickname, key)
    }

    /**
     * The normalized form for uniqueness: the skeleton of [Confusables] without spaces, `-`, `_` and `.`. It is empty only
     * for a name that holds nothing else than those characters.
     */
    fun normalize(nickname: String): String =
        Confusables.skeleton(nickname).filterNot { it.isWhitespace() || it == '-' || it == '_' || it == '.' }

    /** True when the text holds a character that [validate] refuses as hidden. The plain space is not hidden. */
    fun hasHidden(text: String): Boolean {
        var i = 0
        while (i < text.length) {
            val cp = text.codePointAt(i)
            i += Character.charCount(cp)
            if (cp == ' '.code) continue
            if (cp < 0x20 || cp == 0x7F) return true
            val type = Character.getType(cp)
            if (type == Character.UNASSIGNED.toInt() || type == Character.PRIVATE_USE.toInt()) return true
            if (cp >= 0x80 && VisibleText.kindOf(cp, -1, nonAscii = false) != null) return true
        }
        return false
    }

    /**
     * The monogram: the first two letters or digits of the nickname, in upper case. A nickname with one letter gives one.
     * A nickname with no letter and no digit gives its first visible character. The result is never empty for a valid
     * nickname.
     */
    fun monogram(nickname: String): String {
        val picked = StringBuilder()
        var count = 0
        var i = 0
        while (i < nickname.length && count < 2) {
            val cp = nickname.codePointAt(i)
            i += Character.charCount(cp)
            if (Character.isLetterOrDigit(cp)) {
                picked.appendCodePoint(cp)
                count++
            }
        }
        if (picked.isEmpty()) {
            val first = nickname.codePoints().toArray().firstOrNull { it != ' '.code } ?: return "?"
            return String(Character.toChars(first))
        }
        return picked.toString().uppercase(java.util.Locale.ROOT)
    }
}
