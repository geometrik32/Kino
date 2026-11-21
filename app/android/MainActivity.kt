package dev.dioxus.main

import android.graphics.Color
import android.os.Handler
import android.os.Looper
import android.view.Gravity
import android.view.TextureView
import android.view.View
import android.view.ViewGroup
import android.webkit.WebView
import android.widget.FrameLayout
import androidx.annotation.OptIn
import androidx.media3.common.C
import androidx.media3.common.MediaItem
import androidx.media3.common.PlaybackException
import androidx.media3.common.PlaybackParameters
import androidx.media3.common.Player
import androidx.media3.common.TrackSelectionOverride
import androidx.media3.common.Tracks
import androidx.media3.common.VideoSize
import androidx.media3.common.text.CueGroup
import androidx.media3.common.util.UnstableApi
import androidx.media3.datasource.DefaultHttpDataSource
import androidx.media3.exoplayer.DefaultLoadControl
import androidx.media3.exoplayer.DefaultRenderersFactory
import androidx.media3.exoplayer.ExoPlayer
import androidx.media3.exoplayer.source.DefaultMediaSourceFactory
import androidx.media3.ui.AspectRatioFrameLayout
import androidx.media3.ui.SubtitleView

typealias BuildConfig = ru.aatterra.orelkino.BuildConfig

@OptIn(UnstableApi::class)
class MainActivity : WryActivity() {
    private var mWebViewRef: WebView? = null
    private var player: ExoPlayer? = null
    private var playerContainer: FrameLayout? = null
    private var aspectLayout: AspectRatioFrameLayout? = null
    private var textureView: TextureView? = null
    private var subtitleView: SubtitleView? = null

    private val mainHandler = Handler(Looper.getMainLooper())
    private var tickerRunning = false

    @Volatile private var openSeq: Long = 0L
    @Volatile private var seekSeq: Long = 0L
    @Volatile private var currentPosSec: Double = 0.0
    @Volatile private var durationSec: Double = Double.NaN
    @Volatile private var isPaused: Boolean = true
    @Volatile private var isBuffering: Boolean = false
    @Volatile private var isReady: Boolean = false
    @Volatile private var isEnded: Boolean = false
    @Volatile private var hasError: Boolean = false

    private var pendingAudioIdx: Int = -1
    private var pendingSubIdx: Int = -1
    private var userVolume: Float = 1.0f
    private var userMuted: Boolean = false

    private val tickRunnable = object : Runnable {
        override fun run() {
            val exo = player
            if (exo != null && tickerRunning) {
                updateSnapshot(exo)
                mainHandler.postDelayed(this, 200L)
            }
        }
    }

    override fun onWebViewCreate(webView: WebView) {
        mWebViewRef = webView
        webView.post { ensurePlayerContainer() }
    }

    override fun onStop() {
        super.onStop()
        player?.let { exo ->
            if (exo.playWhenReady) {
                exo.pause()
            }
        }
    }

    override fun onDestroy() {
        tickerRunning = false
        mainHandler.removeCallbacks(tickRunnable)
        player?.release()
        player = null
        super.onDestroy()
    }

    private fun ensurePlayerContainer() {
        if (playerContainer != null) return
        val webView = mWebViewRef ?: return
        val parent = webView.parent as? ViewGroup ?: return

        val container = FrameLayout(this).apply {
            setBackgroundColor(Color.BLACK)
            visibility = View.GONE
        }
        val aspect = AspectRatioFrameLayout(this).apply {
            resizeMode = AspectRatioFrameLayout.RESIZE_MODE_FIT
        }
        val tv = TextureView(this)
        val subs = SubtitleView(this).apply {
            setApplyEmbeddedStyles(true)
            setApplyEmbeddedFontSizes(true)
            setUserDefaultStyle()
            setUserDefaultTextSize()
        }

        aspect.addView(
            tv,
            FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT
            )
        )
        aspect.addView(
            subs,
            FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT
            )
        )
        container.addView(
            aspect,
            FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT,
                Gravity.CENTER
            )
        )

        parent.addView(
            container,
            0,
            ViewGroup.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT
            )
        )

        playerContainer = container
        aspectLayout = aspect
        textureView = tv
        subtitleView = subs
    }

    private fun ensurePlayer(): ExoPlayer {
        player?.let { return it }

        val renderersFactory = DefaultRenderersFactory(this)
            .setExtensionRendererMode(DefaultRenderersFactory.EXTENSION_RENDERER_MODE_ON)
        val loadControl = DefaultLoadControl.Builder()
            .setBufferDurationsMs(2_500, 30_000, 1_000, 2_000)
            .build()
        val httpFactory = DefaultHttpDataSource.Factory()
            .setAllowCrossProtocolRedirects(true)
            .setConnectTimeoutMs(15_000)
            .setReadTimeoutMs(30_000)
        val exo = ExoPlayer.Builder(this, renderersFactory)
            .setMediaSourceFactory(DefaultMediaSourceFactory(httpFactory))
            .setLoadControl(loadControl)
            .build()

        exo.addListener(object : Player.Listener {
            override fun onPlaybackStateChanged(playbackState: Int) {
                when (playbackState) {
                    Player.STATE_BUFFERING -> isBuffering = true
                    Player.STATE_READY -> {
                        isBuffering = false
                        isReady = true
                        isEnded = false
                        updateSnapshot(exo)
                    }
                    Player.STATE_ENDED -> {
                        isBuffering = false
                        isEnded = true
                        isPaused = true
                        playerContainer?.keepScreenOn = false
                        updateSnapshot(exo)
                    }
                    Player.STATE_IDLE -> {
                        isBuffering = false
                        playerContainer?.keepScreenOn = false
                    }
                }
            }

            override fun onIsPlayingChanged(isPlaying: Boolean) {
                playerContainer?.keepScreenOn = isPlaying
            }

            override fun onPlayWhenReadyChanged(playWhenReady: Boolean, reason: Int) {
                isPaused = !playWhenReady
            }

            override fun onPositionDiscontinuity(
                oldPosition: Player.PositionInfo,
                newPosition: Player.PositionInfo,
                reason: Int
            ) {
                if (reason == Player.DISCONTINUITY_REASON_SEEK) {
                    seekSeq += 1L
                    currentPosSec = newPosition.positionMs.coerceAtLeast(0L) / 1000.0
                }
            }

            override fun onVideoSizeChanged(videoSize: VideoSize) {
                if (videoSize.width > 0 && videoSize.height > 0) {
                    val ratio = (videoSize.width.toFloat() * videoSize.pixelWidthHeightRatio) /
                        videoSize.height.toFloat()
                    aspectLayout?.setAspectRatio(ratio)
                }
            }

            override fun onCues(cueGroup: CueGroup) {
                subtitleView?.setCues(cueGroup.cues)
            }

            override fun onTracksChanged(tracks: Tracks) {
                applyTrackSelections(exo)
            }

            override fun onPlayerError(error: PlaybackException) {
                isBuffering = false
                hasError = true
                isPaused = true
            }
        })

        textureView?.let { exo.setVideoTextureView(it) }
        player = exo
        return exo
    }

    private fun updateSnapshot(exo: ExoPlayer) {
        val posMs = exo.currentPosition.coerceAtLeast(0L)
        currentPosSec = posMs / 1000.0
        val durMs = exo.duration
        durationSec = if (durMs != C.TIME_UNSET && durMs > 0L) durMs / 1000.0 else Double.NaN
        isPaused = !exo.playWhenReady
    }

    private fun applyTrackSelections(exo: ExoPlayer) {
        val builder = exo.trackSelectionParameters.buildUpon()
        val audioGroups = exo.currentTracks.groups.filter { it.type == C.TRACK_TYPE_AUDIO }
        val flatAudio = mutableListOf<Pair<Tracks.Group, Int>>()
        for (g in audioGroups) {
            for (i in 0 until g.length) {
                flatAudio.add(g to i)
            }
        }
        if (pendingAudioIdx in flatAudio.indices) {
            val (g, idx) = flatAudio[pendingAudioIdx]
            builder.setOverrideForType(TrackSelectionOverride(g.mediaTrackGroup, idx))
        } else {
            builder.clearOverridesOfType(C.TRACK_TYPE_AUDIO)
        }

        val textGroups = exo.currentTracks.groups.filter { it.type == C.TRACK_TYPE_TEXT }
        val flatText = mutableListOf<Pair<Tracks.Group, Int>>()
        for (g in textGroups) {
            for (i in 0 until g.length) {
                flatText.add(g to i)
            }
        }
        if (pendingSubIdx < 0) {
            builder.setTrackTypeDisabled(C.TRACK_TYPE_TEXT, true)
            builder.clearOverridesOfType(C.TRACK_TYPE_TEXT)
            subtitleView?.setCues(emptyList())
        } else if (pendingSubIdx in flatText.indices) {
            val (g, idx) = flatText[pendingSubIdx]
            builder.setTrackTypeDisabled(C.TRACK_TYPE_TEXT, false)
            builder.setOverrideForType(TrackSelectionOverride(g.mediaTrackGroup, idx))
        }
        exo.trackSelectionParameters = builder.build()
    }

    fun kinoNativeOpen(
        seq: Long,
        url: String,
        startSec: Double,
        rate: Float,
        audioIdx: Int,
        subIdx: Int,
        autoplay: Boolean
    ) {
        seekSeq = 0L
        currentPosSec = startSec.coerceAtLeast(0.0)
        durationSec = Double.NaN
        isPaused = !autoplay
        isBuffering = true
        isReady = false
        isEnded = false
        hasError = false
        openSeq = seq

        runOnUiThread {
            if (seq != openSeq) return@runOnUiThread
            ensurePlayerContainer()
            mWebViewRef?.setBackgroundColor(Color.TRANSPARENT)
            playerContainer?.visibility = View.VISIBLE
            subtitleView?.setCues(emptyList())

            val exo = ensurePlayer()
            textureView?.let { exo.setVideoTextureView(it) }
            pendingAudioIdx = audioIdx
            pendingSubIdx = subIdx
            exo.volume = if (userMuted) 0f else userVolume
            exo.playbackParameters = PlaybackParameters(rate.coerceIn(0.25f, 4.0f))

            val startMs = if (startSec > 0.5) (startSec * 1000.0).toLong() else 0L
            exo.setMediaItem(MediaItem.fromUri(url), startMs)
            exo.playWhenReady = autoplay
            exo.prepare()
            applyTrackSelections(exo)

            if (!tickerRunning) {
                tickerRunning = true
                mainHandler.post(tickRunnable)
            }
        }
    }

    fun kinoNativePlay() {
        runOnUiThread {
            player?.let { exo ->
                exo.playWhenReady = true
                exo.play()
                isPaused = false
            }
        }
    }

    fun kinoNativePause() {
        runOnUiThread {
            player?.let { exo ->
                exo.pause()
                isPaused = true
            }
        }
    }

    fun kinoNativeSeek(sec: Double) {
        runOnUiThread {
            player?.let { exo ->
                val ms = (sec.coerceAtLeast(0.0) * 1000.0).toLong()
                currentPosSec = sec.coerceAtLeast(0.0)
                exo.seekTo(ms)
            }
        }
    }

    fun kinoNativeSetRate(rate: Float) {
        runOnUiThread {
            player?.let { exo ->
                exo.playbackParameters = PlaybackParameters(rate.coerceIn(0.25f, 4.0f))
            }
        }
    }

    fun kinoNativeSetVolume(vol: Float) {
        userVolume = vol.coerceIn(0f, 1f)
        runOnUiThread {
            player?.volume = if (userMuted) 0f else userVolume
        }
    }

    fun kinoNativeSetMuted(muted: Boolean) {
        userMuted = muted
        runOnUiThread {
            player?.volume = if (userMuted) 0f else userVolume
        }
    }

    fun kinoNativeSelectAudio(audioIdx: Int) {
        runOnUiThread {
            pendingAudioIdx = audioIdx
            player?.let { applyTrackSelections(it) }
        }
    }

    fun kinoNativeSelectSubtitle(subIdx: Int) {
        runOnUiThread {
            pendingSubIdx = subIdx
            player?.let { applyTrackSelections(it) }
        }
    }

    fun kinoNativeClose(seq: Long) {
        seekSeq = 0L
        isPaused = true
        isBuffering = false
        isReady = false
        isEnded = false
        hasError = false
        openSeq = seq
        runOnUiThread {
            tickerRunning = false
            mainHandler.removeCallbacks(tickRunnable)
            player?.let { exo ->
                exo.stop()
                exo.clearMediaItems()
            }
            subtitleView?.setCues(emptyList())
            playerContainer?.keepScreenOn = false
            playerContainer?.visibility = View.GONE
            mWebViewRef?.setBackgroundColor(Color.TRANSPARENT)
        }
    }

    fun kinoNativePoll(): String {
        val b = { v: Boolean -> if (v) 1 else 0 }
        return "$openSeq|$seekSeq|$currentPosSec|$durationSec|${b(isPaused)}|${b(isBuffering)}|${b(isReady)}|${b(isEnded)}|${b(hasError)}"
    }
}
