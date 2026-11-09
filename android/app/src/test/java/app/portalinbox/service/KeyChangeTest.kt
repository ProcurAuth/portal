package app.portalinbox.service

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class KeyChangeTest {
    @Test
    fun theNativeMessageOfAnAuthenticationFailureIsAKeyFailure() {
        // The text of `inbox_core::Error::AuthenticationFailed`, as the bridge throws it.
        assertTrue(KeyChange.isHandshakeKeyFailure("authentication failed"))
    }

    @Test
    fun otherNativeFailuresAreNotAKeyChange() {
        for (m in listOf(null, "", "malformed message: noise", "unexpected message: handshake already finished", "unsupported protocol version 9", "inbox native panic")) {
            assertFalse("$m", KeyChange.isHandshakeKeyFailure(m))
        }
    }

    @Test
    fun theStoredReasonGivesTheKeyChangedState() {
        val reason = KeyChange.reason("The Portal did not prove the key")
        assertTrue(KeyChange.isKeyChanged(reason))
        assertEquals(LinkState.KEY_CHANGED, KeyChange.stateOf(reason))
        assertEquals("The Portal did not prove the key", KeyChange.detail(reason))
    }

    @Test
    fun otherReasonsStayInReenroll() {
        assertEquals(LinkState.REENROLL, KeyChange.stateOf("Device revoked by the administrator: delete this server and add it again"))
        assertEquals(LinkState.REENROLL, KeyChange.stateOf("Invalid address"))
        assertFalse(KeyChange.isKeyChanged("Invalid address"))
        assertEquals("Invalid address", KeyChange.detail("Invalid address"))
    }

    @Test
    fun noReasonHasNoState() {
        assertNull(KeyChange.stateOf(null))
        assertFalse(KeyChange.isKeyChanged(null))
    }
}
