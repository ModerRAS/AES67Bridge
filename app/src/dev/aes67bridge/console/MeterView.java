package dev.aes67bridge.console;

import android.content.Context;
import android.graphics.Canvas;
import android.graphics.Color;
import android.graphics.Paint;
import android.graphics.RectF;
import android.view.View;

public class MeterView extends View {
    private float[] levels = new float[8];
    private int ch = 0;
    private Paint bg = new Paint();
    private Paint bar = new Paint();
    private Paint txt = new Paint();
    private Paint border = new Paint();

    public MeterView(Context c) {
        super(c);
        bg.setColor(Color.rgb(20, 26, 32));
        bar.setColor(Color.rgb(60, 200, 140));
        border.setColor(Color.rgb(60, 70, 80));
        border.setStyle(Paint.Style.STROKE);
        border.setStrokeWidth(2f);
        txt.setColor(Color.rgb(200, 210, 220));
        txt.setTextSize(26f);
        txt.setAntiAlias(true);
        bar.setAntiAlias(true);
        setMinimumHeight(260);
    }

    public void setLevels(int channels, float[] lv) {
        ch = Math.max(0, Math.min(8, channels));
        for (int i = 0; i < 8; i++) levels[i] = (i < lv.length) ? lv[i] : -120f;
        invalidate();
    }

    @Override protected void onDraw(Canvas c) {
        super.onDraw(c);
        float w = getWidth(), h = getHeight();
        float gap = 10f;
        int n = Math.max(ch, 1);
        float bw = (w - gap * (n + 1)) / n;
        for (int i = 0; i < n; i++) {
            float x = gap + i * (bw + gap);
            RectF r = new RectF(x, 30, x + bw, h - 40);
            c.drawRect(r, bg);
            c.drawRect(r, border);
            float db = levels[i];
            float norm = (db + 60f) / 60f;
            if (norm < 0) norm = 0; if (norm > 1) norm = 1;
            float bh = (r.height() - 8) * norm;
            RectF fill = new RectF(r.left + 4, r.bottom - 4 - bh, r.right - 4, r.bottom - 4);
            c.drawRect(fill, bar);
            c.drawText((i + 1) + "ch", x + 2, 24, txt);
            c.drawText(Math.round(db) + "", x + 2, h - 12, txt);
        }
        if (n == 1 && ch == 0) c.drawText("no stream", 20, h / 2, txt);
    }
}
