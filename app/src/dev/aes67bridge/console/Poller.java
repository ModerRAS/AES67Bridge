package dev.aes67bridge.console;

import android.os.Handler;
import android.os.Looper;

import java.io.File;
import java.io.FileInputStream;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

public class Poller implements Runnable {
    public interface Sink { void onUpdate(State s); }
    private final Sink sink;
    private final Runnable uiTask;
    private volatile boolean run = true;
    private volatile State latest;
    private final Handler main = new Handler(Looper.getMainLooper());
    private static final Pattern P_RATE = Pattern.compile("\"rate\"\\s*:\\s*(\\d+)");
    private static final Pattern P_CH = Pattern.compile("\"ch\"\\s*:\\s*(\\d+)");
    private static final Pattern P_CODEC = Pattern.compile("\"codec\"\\s*:\\s*\"([a-zA-Z0-9_]+)\"");
    private static final Pattern P_PKT = Pattern.compile("\"packets\"\\s*:\\s*(\\d+)");
    private static final Pattern P_ERR = Pattern.compile("\"send_err\"\\s*:\\s*(\\d+)");
    private static final Pattern P_MODE = Pattern.compile("\"mode\"\\s*:\\s*\"([a-z]+)\"");
    private static final Pattern P_RSIN = Pattern.compile("\"rs_in\"\\s*:\\s*(\\d+)");
    private static final Pattern P_RSOUT = Pattern.compile("\"rs_out\"\\s*:\\s*(\\d+)");
    private static final Pattern P_LV = Pattern.compile("\"levels\"\\s*:\\s*\\[([^\\]]*)\\]");

    public Poller(Sink s, Runnable ui) { sink = s; uiTask = ui; }
    public State getLatest() { return latest; }
    public void stop() { run = false; }
    public void run() {
        while (run) {
            try { latest = read(); } catch (Throwable t) { latest = new State(); }
            try { main.post(uiTask); } catch (Throwable t) { }
            try { Thread.sleep(1000); } catch (InterruptedException e) { break; }
        }
    }
    static String readFile(String path, int max) {
        try {
            File f = new File(path);
            if (!f.exists()) return null;
            FileInputStream in = new FileInputStream(f);
            byte[] buf = new byte[max];
            int n = in.read(buf);
            in.close();
            if (n <= 0) return null;
            return new String(buf, 0, n);
        } catch (Throwable t) { return null; }
    }
    static String grab(Pattern p, String s) {
        if (s == null) return "";
        try { Matcher m = p.matcher(s); return m.find() ? m.group(1) : ""; } catch (Throwable t) { return ""; }
    }
    public static State read() {
        State st = new State();
        String js = readFile("/data/haltee/status.json", 16384);
        if (js != null) {
            st.hasData = true;
            st.rate = grab(P_RATE, js); st.ch = grab(P_CH, js); st.codec = grab(P_CODEC, js);
            st.packets = grab(P_PKT, js); st.err = grab(P_ERR, js); st.mode = grab(P_MODE, js);
            st.rsIn = grab(P_RSIN, js); st.rsOut = grab(P_RSOUT, js);
            try {
                Matcher lm = P_LV.matcher(js);
                if (lm.find()) {
                    String[] parts = lm.group(1).split(",");
                    st.levels = new float[parts.length];
                    for (int i = 0; i < parts.length; i++) {
                        try { st.levels[i] = Float.parseFloat(parts[i].trim()); } catch (Throwable t) { st.levels[i] = -120f; }
                    }
                }
            } catch (Throwable t) { }
        }
        st.boot = readFile("/data/aes67/status.txt", 4096);
        st.meta = readFile("/data/haltee/live.meta", 4096);
        return st;
    }
    public static class State {
        public boolean hasData;
        public String rate = "", ch = "", codec = "", packets = "", err = "", mode = "", rsIn = "", rsOut = "";
        public String boot = null, meta = null;
        public float[] levels = new float[0];
    }
}
