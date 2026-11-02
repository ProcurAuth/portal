package app.portalinbox.net

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class AddressTest {
    @Test
    fun anHttpAddressStaysPlain() {
        val a = ServerAddress.parse("http://172.17.0.1:8088")!!
        assertFalse(a.secure)
        assertEquals("ws://172.17.0.1:8088/v1/enroll", a.ws("/v1/enroll"))
    }

    @Test
    fun anHttpsAddressUsesTheSecureWebSocket() {
        val a = ServerAddress.parse("https://portal.example.com/p/")!!
        assertTrue(a.secure)
        assertEquals("https://portal.example.com/p", a.http)
        assertEquals("wss://portal.example.com/p/v1/session", a.ws("/v1/session"))
    }

    @Test
    fun aBareHostGetsHttp() {
        assertEquals("http://host:8080", ServerAddress.parse("host:8080")!!.http)
    }

    @Test
    fun aBadAddressIsRefused() {
        for (bad in listOf("", "a b", "ftp://host", "http://host?x=1", "http://u@host", "http://host#f")) {
            assertNull(bad, ServerAddress.parse(bad))
        }
    }
}
