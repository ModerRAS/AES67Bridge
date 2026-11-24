// NsdAdvertiser.kt - RAVENNA DNS-SD advertisement through Android NsdManager.
//
// STATUS: written for review only. The RAVENNA track is paused (2026-11-24) and
// this file has NOT been compiled or run inside the AES67Bridge Compose app.
//
// Why NsdManager instead of a Rust mDNS responder:
//   On the Z9X, UDP/5353 is already owned by the Android system mDNS daemon
//   (4 sockets, UID 1000). A normal app cannot take over 5353, so the device side
//   must go through the system resolver: NsdManager.
//
// The hard constraint (verified 2026-11-24):
//   NsdServiceInfo.setSubtypes/getSubtypes were added in API level 35 (Android 15).
//   https://android-dotdevsite-v2-prod.appspot.com/sdk/api_diff/35/changes/android.net.nsd.NsdServiceInfo
//   Z9X 8K runs Android 11 (API 30) => no public subtype registration.
//
// RAVENNA needs subtypes:
//   node RTSP service  : _rtsp._tcp        + _ravenna._sub._rtsp._tcp
//   node HTTP service  : _http._tcp        + _ravenna._sub._http._tcp
//   session service    : _rtsp._tcp        + _ravenna_session._sub._rtsp._tcp
//   (RAVENNA Operating Principles 3.5.1 / 3.5.2.1)
//
// So on Android 11 this class can only publish the BASE types unless the
// undocumented composite-type hack works on the target build (see USE_COMPOSITE_HACK).
// A controller that browses _ravenna_session._sub._rtsp._tcp will NOT see us in
// that case; see android/README.md for the fallback options.

package com.aes67bridge.ravenna

import android.content.Context
import android.net.nsd.NsdManager
import android.net.nsd.NsdServiceInfo
import android.os.Build
import android.util.Log

class NsdAdvertiser(private val context: Context) {

    companion object {
        private const val TAG = "NsdAdvertiser"

        /** RTSP port of the Rust daemon on the device. */
        const val RTSP_PORT = 8554
        /** Placeholder for a future HTTP config page. */
        const val HTTP_PORT = 8080

        /** RAVENNA subtype labels, without the trailing "._sub" (as the API expects). */
        const val SUBTYPE_NODE = "_ravenna"
        const val SUBTYPE_SESSION = "_ravenna_session"

        /**
         * Undocumented Android < 15 behaviour: the bundled mDNSResponder accepts a
         * composite service type such as "_ravenna_session._sub._rtsp._tcp" and
         * registers the subtype. connectedhomeip used this in PR #15177 and noted
         * it "MAY STOP WORKING in future Android versions".
         * Off by default: it is unverified on this firmware.
         */
        private const val USE_COMPOSITE_HACK = false
        private const val REGISTRATION_GAP_MS = 250L
    }

    private val nsdManager: NsdManager =
        context.getSystemService(Context.NSD_SERVICE) as NsdManager

    private val registrations = mutableMapOf<String, NsdManager.RegistrationListener>()

    /** Publish the node RTSP service (the service a RAVENNA source MUST publish). */
    fun advertiseNode(nodeId: String, txt: Map<String, String> = emptyMap()) {
        register(
            key = "node-rtsp",
            instanceName = nodeId,
            baseType = "_rtsp._tcp",
            subtype = SUBTYPE_NODE,
            port = RTSP_PORT,
            txt = txt + mapOf("node-id" to nodeId),
        )
        // RECOMMENDED for any node: the HTTP configuration service.
        register(
            key = "node-http",
            instanceName = nodeId,
            baseType = "_http._tcp",
            subtype = SUBTYPE_NODE,
            port = HTTP_PORT,
            txt = mapOf("node-id" to nodeId),
        )
    }

    /** Publish one streaming session: instance name = unique session name. */
    fun advertiseSession(sessionName: String, txt: Map<String, String> = emptyMap()) {
        register(
            key = "session:" + sessionName,
            instanceName = sessionName,
            baseType = "_rtsp._tcp",
            subtype = SUBTYPE_SESSION,
            port = RTSP_PORT,
            txt = txt,
        )
    }

    /** Withdraw every service (call from onPause/onDestroy). */
    fun stopAll() {
        registrations.forEach { (_, listener) ->
            runCatching { nsdManager.unregisterService(listener) }
                .onFailure { Log.w(TAG, "unregister failed: " + it.message) }
        }
        registrations.clear()
    }

    private fun register(
        key: String,
        instanceName: String,
        baseType: String,
        subtype: String,
        port: Int,
        txt: Map<String, String>,
    ) {
        if (registrations.containsKey(key)) return
        val info = NsdServiceInfo().apply {
            serviceName = instanceName
            serviceType = serviceTypeFor(baseType, subtype)
            setPort(port)
            txt.forEach { (k, v) -> setAttribute(k, v) }
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.VANILLA_ICE_CREAM) {
                // API 35 (Android 15) and newer: the official way.
                subtypes = setOf(subtype)
            }
        }
        val listener = object : NsdManager.RegistrationListener {
            override fun onServiceRegistered(info: NsdServiceInfo) {
                // Android may rename the instance on conflict; this is exactly what
                // RAVENNA 3.5.1.2 says a device should NOT do silently, so log it.
                if (info.serviceName != instanceName) {
                    Log.w(TAG, "instance renamed by system: " + instanceName + " -> " + info.serviceName)
                }
                Log.i(TAG, "registered " + info.serviceName + " " + info.serviceType + ":" + port)
            }

            override fun onRegistrationFailed(info: NsdServiceInfo, errorCode: Int) {
                Log.e(TAG, "registration failed for " + instanceName + " code=" + errorCode)
            }

            override fun onServiceUnregistered(info: NsdServiceInfo) {
                Log.i(TAG, "unregistered " + info.serviceName)
            }

            override fun onUnregistrationFailed(info: NsdServiceInfo, errorCode: Int) {
                Log.e(TAG, "unregistration failed code=" + errorCode)
            }
        }
        registrations[key] = listener
        // NsdManager is historically flaky when registrations are issued back to back.
        android.os.Handler(android.os.Looper.getMainLooper()).postDelayed({
            runCatching { nsdManager.registerService(info, NsdManager.PROTOCOL_DNS_SD, listener) }
                .onFailure { Log.e(TAG, "registerService threw: " + it.message) }
        }, REGISTRATION_GAP_MS * registrations.size)
    }

    /**
     * Android < 15 has no subtype field. Two options:
     *  - default: publish the base type only (a subtype browser will NOT find us);
     *  - USE_COMPOSITE_HACK: pass "_sub._type._tcp" as the service type, which the
     *    system mDNSResponder may accept as a subtype registration (undocumented).
     */
    private fun serviceTypeFor(baseType: String, subtype: String): String {
        val wantsSubtype = Build.VERSION.SDK_INT < Build.VERSION_CODES.VANILLA_ICE_CREAM
        return if (USE_COMPOSITE_HACK && wantsSubtype) {
            subtype + "._sub." + baseType
        } else {
            baseType
        }
    }
}
