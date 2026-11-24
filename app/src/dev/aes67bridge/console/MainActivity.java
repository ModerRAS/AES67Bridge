package dev.aes67bridge.console;

import android.app.Activity;
import android.content.Intent;
import android.graphics.Color;
import android.os.Bundle;
import android.view.Gravity;
import android.view.View;
import android.view.ViewGroup;
import android.widget.Button;
import android.widget.LinearLayout;
import android.widget.TextView;

public class MainActivity extends Activity implements View.OnClickListener, Poller.Sink, Runnable {
    TextView head, metrics, log;
    MeterView meters;
    Button bEnable, bDisable, bLow, bNorm, bRollback, bRefresh;
    Poller poller;

    @Override protected void onCreate(Bundle b) {
        super.onCreate(b);
        LinearLayout root = new LinearLayout(this);
        root.setOrientation(LinearLayout.VERTICAL);
        root.setBackgroundColor(Color.rgb(12, 16, 20));
        root.setPadding(48, 32, 48, 24);
        head = new TextView(this);
        head.setText("AES67Bridge · Z9X 8K");
        head.setTextColor(Color.rgb(230, 240, 250));
        head.setTextSize(30);
        head.setGravity(Gravity.CENTER_HORIZONTAL);
        root.addView(head, new LinearLayout.LayoutParams(-1, -2));
        metrics = new TextView(this);
        metrics.setTextColor(Color.rgb(180, 200, 220));
        metrics.setTextSize(17);
        metrics.setPadding(0, 14, 0, 6);
        root.addView(metrics, new LinearLayout.LayoutParams(-1, -2));
        meters = new MeterView(this);
        root.addView(meters, new LinearLayout.LayoutParams(-1, 0, 3));
        LinearLayout row = new LinearLayout(this);
        row.setOrientation(LinearLayout.HORIZONTAL);
        bEnable = mkBtn("启动"); bDisable = mkBtn("停止");
        bLow = mkBtn("低延迟 5ms"); bNorm = mkBtn("标准 20ms");
        bRollback = mkBtn("回滚"); bRefresh = mkBtn("刷新");
        row.addView(bEnable); row.addView(bDisable); row.addView(bLow); row.addView(bNorm); row.addView(bRollback); row.addView(bRefresh);
        root.addView(row);
        log = new TextView(this);
        log.setTextColor(Color.rgb(140, 160, 180));
        log.setTextSize(13);
        log.setPadding(0, 8, 0, 0);
        root.addView(log, new LinearLayout.LayoutParams(-1, 0, 2));
        setContentView(root);
        poller = new Poller(this, this);
        new Thread(poller).start();
    }

    private Button mkBtn(String t) {
        Button b = new Button(this);
        b.setText(t);
        b.setTextSize(16);
        b.setOnClickListener(this);
        return b;
    }

    @Override protected void onDestroy() { if (poller != null) poller.stop(); super.onDestroy(); }

    @Override public void onClick(View v) {
        Intent i = new Intent(this, AutoStartService.class);
        if (v == bEnable) i.setAction("ENABLE");
        else if (v == bDisable || v == bRollback) i.setAction("DISABLE");
        else if (v == bLow) i.setAction("LOWLAT");
        else if (v == bNorm) i.setAction("NORMAL");
        else { onUpdate(Poller.read()); return; }
        startForegroundService(i);
        log.setText("已下发 " + i.getAction() + " ...");
    }

    @Override public void run() {
        Poller.State s = poller == null ? null : poller.getLatest();
        if (s != null) onUpdate(s);
    }

    @Override public void onUpdate(Poller.State s) {
        StringBuilder sb = new StringBuilder();
        sb.append("状态: ").append(s.hasData ? "● 运行中" : "○ 无数据");
        sb.append("    模式: ").append(s.mode.isEmpty() ? "-" : s.mode);
        sb.append("\\n格式: ").append(s.codec.isEmpty() ? "-" : s.codec)
          .append("  ").append(s.rate.isEmpty() ? "-" : s.rate).append(" Hz")
          .append("  ").append(s.ch.isEmpty() ? "-" : s.ch).append(" ch");
        sb.append("    源: ").append(s.rsIn.isEmpty() ? "-" : ("SRC " + s.rsIn + "→" + s.rsOut));
        sb.append("\\nRTP: ").append(s.packets.isEmpty() ? "-" : (s.packets + " 包"))
          .append("  发送错误: ").append(s.err.isEmpty() ? "-" : s.err)
          .append("    目标 239.69.2.1:5004 PT97");
        metrics.setText(sb.toString());
        int nch = 0;
        try { nch = Integer.parseInt(s.ch); } catch (Exception e) { }
        meters.setLevels(nch, s.levels);
        StringBuilder lg = new StringBuilder();
        if (s.meta != null) lg.append("meta: ").append(s.meta.replace("\\n", "  "));
        log.setText(lg.toString());
    }
}
