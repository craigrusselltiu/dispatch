package com.dispatch.radio.ui

import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.util.AttributeSet
import android.view.View
import androidx.core.content.ContextCompat
import com.dispatch.radio.R

/**
 * Segmented audio level bar shown during PTT (dispatch-88k.6).
 * Renders ~20 equal segments; filled segments are lit green, empty ones are dimmed.
 */
class AudioLevelView @JvmOverloads constructor(
    context: Context,
    attrs: AttributeSet? = null,
    defStyleAttr: Int = 0,
) : View(context, attrs, defStyleAttr) {

    private val fillPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = ContextCompat.getColor(context, R.color.green)
    }
    private val emptyPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = 0xFF2A2A2A.toInt()
    }

    private val segmentCount = 20
    private val segmentGap = 2f

    /** Audio level from 0.0 to 1.0. */
    var level: Float = 0f
        set(value) {
            field = value.coerceIn(0f, 1f)
            invalidate()
        }

    override fun onDraw(canvas: Canvas) {
        super.onDraw(canvas)
        val w = width.toFloat()
        val h = height.toFloat()
        val totalGaps = segmentGap * (segmentCount - 1)
        val segW = (w - totalGaps) / segmentCount
        val filledCount = (level * segmentCount).toInt()

        for (i in 0 until segmentCount) {
            val left = i * (segW + segmentGap)
            val paint = if (i < filledCount) fillPaint else emptyPaint
            canvas.drawRect(left, 0f, left + segW, h, paint)
        }
    }
}
