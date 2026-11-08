package app.portalinbox.proto

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import java.math.BigDecimal

class StrictJsonTest {
    @Test
    fun numbersKeepTheirKind() {
        val o = StrictJson.parseObject("""{"a":1,"b":-9223372036854775808,"c":1.5,"d":1e2,"e":-0,"f":18446744073709551616,"g":0}""")
        assertEquals(1L, o.get("a"))
        assertEquals(Long.MIN_VALUE, o.get("b"))
        assertEquals(BigDecimal("1.5"), o.get("c"))
        assertEquals(BigDecimal("1e2"), o.get("d"))
        assertEquals(-0.0, o.get("e") as Double, 0.0)
        assertTrue(1.0 / (o.get("e") as Double) < 0)
        assertEquals(BigDecimal("18446744073709551616"), o.get("f"))
        assertEquals(0L, o.get("g"))
    }

    @Test
    fun theWriterIsCompactAndSorted() {
        val text = "q\"\\\b\t\n\u000c\r\u0001\u001f \u00e9 \u2028 \uD83D\uDE00"
        val o = JSONObject().put("b", 1L).put("a", JSONArray().put("x/y").put(JSONObject.NULL).put(true).put(2L)).put("c", text)
        val expected = "{\"a\":[\"x/y\",null,true,2],\"b\":1,\"c\":\"q\\\"\\\\\\b\\t\\n\\f\\r\\u0001\\u001f \u00e9 \u2028 \uD83D\uDE00\"}"
        assertEquals(expected, StrictJson.compact(o))
        // What the writer writes, the reader reads back.
        assertTrue(o.similar(StrictJson.parseObject(StrictJson.compact(o))))
    }

    @Test
    fun theWriterRefusesWhatJsonCannotHold() {
        try {
            StrictJson.compact(JSONObject().put("x", 0.5).put("y", Any()))
            throw AssertionError("an unknown type was written")
        } catch (e: FrameException) {
        }
    }

    @Test
    fun whiteSpaceBetweenTokensIsAllowedAndOnlyThat() {
        assertEquals(1L, StrictJson.parseObject(" \t\r\n{ \"a\" :\n1 }\n ").get("a"))
        try {
            StrictJson.parse("{\"a\":1}\u000b")
            throw AssertionError("a vertical tab was accepted")
        } catch (e: FrameException) {
        }
    }
}
