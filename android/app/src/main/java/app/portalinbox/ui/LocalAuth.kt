package app.portalinbox.ui

import android.content.Context
import android.os.Build
import androidx.biometric.BiometricManager

/** The local gate before an approval or an answer: a strong biometric or the credential of the device ([BiometricPolicy]). */
object LocalAuth {
    fun authenticators(sdkInt: Int = Build.VERSION.SDK_INT): Int = when (BiometricPolicy.allowed(sdkInt)) {
        BiometricPolicy.Allowed.STRONG_OR_CREDENTIAL ->
            BiometricManager.Authenticators.BIOMETRIC_STRONG or BiometricManager.Authenticators.DEVICE_CREDENTIAL
        BiometricPolicy.Allowed.WEAK_OR_CREDENTIAL ->
            BiometricManager.Authenticators.BIOMETRIC_WEAK or BiometricManager.Authenticators.DEVICE_CREDENTIAL
    }

    fun canAuthenticate(context: Context): Boolean =
        BiometricManager.from(context).canAuthenticate(authenticators()) == BiometricManager.BIOMETRIC_SUCCESS
}
