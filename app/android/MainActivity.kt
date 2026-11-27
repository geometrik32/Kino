package dev.dioxus.main

import android.content.Intent
import android.content.pm.ActivityInfo
import android.graphics.Color
import android.media.AudioManager
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.provider.Settings
import android.util.Log
import android.view.Gravity
import android.view.TextureView
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.webkit.WebView
import android.widget.FrameLayout
import androidx.activity.OnBackPressedCallback
import androidx.annotation.OptIn
import androidx.core.content.FileProvider
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import androidx.media3.common.AudioAttributes
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
import java.io.File
import java.util.Locale

typealias BuildConfig = ru.aatterra.orelkino.BuildConfig

@OptIn(UnstableApi::class)
class MainActivity : WryActivity() {
    companion object {
        private const val TAG = "KinoExo"
        private const val DARK_BAR_COLOR = 0xFF0E1621.toInt()
        private const val NO_VIDEO_FRAME_TIMEOUT_MS = 5_000L
    }

    private var mWebViewRef: WebView? = null
    private var player: ExoPlayer? = null
    private var playerContainer: FrameLayout? = null
    private var aspectLayout: AspectRatioFrameLayout? = null
    private var textureView: TextureView? = null
    private var subtitleView: SubtitleView? = null

    private val mainHandler = Handler(Looper.getMainLooper())
    private var tickerRunning = false
    private var backCallback: OnBackPressedCallback? = null

    private var isPlayerMode = false
    private var isLightBars = false
    private var currentBarColor: Int = DARK_BAR_COLOR
    private var lastCssInsets: Array<String>? = null
    private var pendingInstallApkPath: String? = null
    private var awaitingInstallPermission = false

    @Volatile private var openSeq: Long = 0L
    @Volatile private var seekSeq: Long = 0L
    @Volatile private var currentPosSec: Double = 0.0
    @Volatile private var durationSec: Double = Double.NaN
    @Volatile private var isPaused: Boolean = true
    @Volatile private var isBuffering: Boolean = false
    @Volatile private var isReady: Boolean = false
    @Volatile private var isEnded: Boolean = false
    @Volatile private var hasError: Boolean = false

    @Volatile private var hasVideoTrack: Boolean = false
    @Volatile private var firstFrameRendered: Boolean = false
    @Volatile private var playbackStartedAtMs: Long = 0L

    private var pendingAudioIdx: Int = -1
    private var pendingSubIdx: Int = -1
    private var userVolume: Float = 1.0f
    private var userMuted: Boolean = false
    @Volatile private var videoScaleMode: Int = 0
    @Volatile private var videoRatio: Float = 0f
    @Volatile private var cachedEffectiveScale: Int = 1

    private val tickRunnable = object : Runnable {
        override fun run() {
            val exo = player
            if (exo != null && tickerRunning) {
                updateSnapshot(exo)
                if (isReady && !isPaused && !isBuffering && !isEnded && !hasError &&
                    hasVideoTrack && !firstFrameRendered && playbackStartedAtMs > 0L
                ) {
                    val elapsed = SystemClock.elapsedRealtime() - playbackStartedAtMs
                    if (elapsed >= NO_VIDEO_FRAME_TIMEOUT_MS) {
                        Log.w(TAG, "No video frame rendered in ${elapsed}ms; falling back to transcode")
                        triggerNativeFailure(exo)
                    }
                }
                mainHandler.postDelayed(this, 200L)
            }
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        volumeControlStream = AudioManager.STREAM_MUSIC
        runCatching {
            File(cacheDir, "updates").listFiles()?.forEach { it.delete() }
        }
        WindowCompat.setDecorFitsSystemWindows(window, false)
        applyWindowBarAppearance()
        val cb = object : OnBackPressedCallback(false) {
            override fun handleOnBackPressed() {
                mWebViewRef?.post {
                    mWebViewRef?.evaluateJavascript(
                        "if(window.__kinoNativeBack){window.__kinoNativeBack();}",
                        null
                    )
                }
            }
        }
        backCallback = cb
        onBackPressedDispatcher.addCallback(this, cb)
        findViewById<View>(android.R.id.content)?.let { content ->
            ViewCompat.setOnApplyWindowInsetsListener(content) { view, windowInsets ->
                val bars = windowInsets.getInsets(
                    WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout()
                )
                val ime = windowInsets.getInsets(WindowInsetsCompat.Type.ime())
                val cut = windowInsets.getInsets(WindowInsetsCompat.Type.displayCutout())
                if (isPlayerMode) {
                    view.setPadding(0, 0, 0, 0)
                    // Pass symmetric horizontal camera cutout margins into CSS so controls stay centered;
                    // hidden nav bar width stays excluded.
                    val symCut = maxOf(cut.left, cut.right)
                    applyCssInsets(symCut, cut.top, symCut, cut.bottom)
                } else {
                    val symBars = maxOf(bars.left, bars.right)
                    view.setPadding(symBars, bars.top, symBars, maxOf(bars.bottom, ime.bottom))
                    applyCssInsets(0, 0, 0, 0)
                }
                WindowInsetsCompat.CONSUMED
            }
        }
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        if (hasFocus && isPlayerMode) {
            val ctrl = WindowCompat.getInsetsController(window, window.decorView)
            ctrl.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
            ctrl.hide(WindowInsetsCompat.Type.systemBars())
        }
    }

    fun kinoSetBackEnabled(enabled: Boolean) {
        runOnUiThread {
            backCallback?.isEnabled = enabled
        }
    }

    override fun onWebViewCreate(webView: WebView) {
        mWebViewRef = webView
        webView.settings.apply {
            allowFileAccess = false
            allowContentAccess = false
            @Suppress("DEPRECATION")
            run {
                allowFileAccessFromFileURLs = false
                allowUniversalAccessFromFileURLs = false
                setGeolocationEnabled(false)
            }
            javaScriptCanOpenWindowsAutomatically = false
            if (Build.VERSION.SDK_INT >= 26) {
                safeBrowsingEnabled = true
            }
        }
        webView.isVerticalScrollBarEnabled = false
        webView.isHorizontalScrollBarEnabled = false
        webView.setBackgroundColor(Color.TRANSPARENT)
        webView.setLayerType(View.LAYER_TYPE_HARDWARE, null)
        webView.post {
            ensurePlayerContainer()
            applyLastCssInsets()
            findViewById<View>(android.R.id.content)?.let { ViewCompat.requestApplyInsets(it) }
        }
    }

    override fun onResume() {
        super.onResume()
        if (awaitingInstallPermission) {
            awaitingInstallPermission = false
            val path = pendingInstallApkPath
            if (path != null && (Build.VERSION.SDK_INT < Build.VERSION_CODES.O || packageManager.canRequestPackageInstalls())) {
                pendingInstallApkPath = null
                launchApkInstall(path)
            }
        }
    }

    fun kinoCacheDir(): String = cacheDir.absolutePath

    fun kinoInstallApk(path: String): Boolean {
        val file = File(path)
        if (!file.isFile) return false
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O && !packageManager.canRequestPackageInstalls()) {
            pendingInstallApkPath = path
            awaitingInstallPermission = true
            runOnUiThread {
                val intent = Intent(
                    Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                    Uri.parse("package:$packageName")
                )
                startActivity(intent)
            }
            return false
        }
        runOnUiThread {
            launchApkInstall(path)
        }
        return true
    }

    private fun launchApkInstall(path: String) {
        try {
            val file = File(path)
            if (!file.isFile) return
            val uri = FileProvider.getUriForFile(this, "$packageName.fileprovider", file)
            val intent = Intent(Intent.ACTION_VIEW).apply {
                setDataAndType(uri, "application/vnd.android.package-archive")
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_GRANT_READ_URI_PERMISSION)
            }
            startActivity(intent)
        } catch (e: Exception) {
            Log.e(TAG, "Failed to start APK installer", e)
        }
    }

    override fun onStart() {
        super.onStart()
        if (player != null && !tickerRunning) {
            tickerRunning = true
            mainHandler.post(tickRunnable)
        }
    }

    override fun onStop() {
        super.onStop()
        tickerRunning = false
        mainHandler.removeCallbacks(tickRunnable)
        player?.let { exo ->
            if (exo.playWhenReady) {
                exo.pause()
            }
        }
    }

    override fun onDestroy() {
        tickerRunning = false
        mainHandler.removeCallbacksAndMessages(null)
        player?.release()
        player = null
        mWebViewRef = null
        playerContainer = null
        aspectLayout = null
        textureView = null
        subtitleView = null
        backCallback = null
        super.onDestroy()
    }

    fun kinoStyleBars(light: Boolean, barColor: Int) {
        isLightBars = light
        currentBarColor = barColor
        runOnUiThread {
            applyWindowBarAppearance()
        }
    }

    fun kinoSetPlayerMode(on: Boolean) {
        isPlayerMode = on
        runOnUiThread {
            requestedOrientation = if (on) {
                ActivityInfo.SCREEN_ORIENTATION_SENSOR_LANDSCAPE
            } else {
                ActivityInfo.SCREEN_ORIENTATION_UNSPECIFIED
            }
            if (Build.VERSION.SDK_INT >= 28) {
                val attrs = window.attributes
                attrs.layoutInDisplayCutoutMode = if (on) {
                    WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_SHORT_EDGES
                } else {
                    WindowManager.LayoutParams.LAYOUT_IN_DISPLAY_CUTOUT_MODE_DEFAULT
                }
                window.attributes = attrs
            }
            applyWindowBarAppearance()
            val ctrl = WindowCompat.getInsetsController(window, window.decorView)
            if (on) {
                ctrl.systemBarsBehavior = WindowInsetsControllerCompat.BEHAVIOR_SHOW_TRANSIENT_BARS_BY_SWIPE
                ctrl.hide(WindowInsetsCompat.Type.systemBars())
            } else {
                ctrl.show(WindowInsetsCompat.Type.systemBars())
            }
            findViewById<View>(android.R.id.content)?.let { content ->
                ViewCompat.requestApplyInsets(content)
            }
            mainHandler.postDelayed({ applyLastCssInsets() }, 120L)
        }
    }

    private fun applyWindowBarAppearance() {
        val bg = if (isPlayerMode) Color.BLACK else currentBarColor
        window.decorView.setBackgroundColor(bg)
        findViewById<View>(android.R.id.content)?.setBackgroundColor(bg)
        @Suppress("DEPRECATION")
        run {
            window.statusBarColor = if (isPlayerMode) Color.TRANSPARENT else currentBarColor
            window.navigationBarColor = if (isPlayerMode) Color.TRANSPARENT else currentBarColor
        }
        if (Build.VERSION.SDK_INT >= 29) {
            window.isNavigationBarContrastEnforced = false
        }
        val ctrl = WindowCompat.getInsetsController(window, window.decorView)
        val light = !isPlayerMode && isLightBars
        ctrl.isAppearanceLightStatusBars = light
        ctrl.isAppearanceLightNavigationBars = light
    }

    private fun applyCssInsets(leftPx: Int, topPx: Int, rightPx: Int, bottomPx: Int) {
        val d = resources.displayMetrics.density.coerceAtLeast(1f)
        val vals = arrayOf(
            String.format(Locale.US, "%.1fpx", leftPx / d),
            String.format(Locale.US, "%.1fpx", topPx / d),
            String.format(Locale.US, "%.1fpx", rightPx / d),
            String.format(Locale.US, "%.1fpx", bottomPx / d)
        )
        lastCssInsets = vals
        applyLastCssInsets()
    }

    private fun applyLastCssInsets() {
        val webView = mWebViewRef ?: return
        val vals = lastCssInsets ?: return
        val js = "(function(){var s=document.documentElement&&document.documentElement.style;if(!s)return;" +
            "s.setProperty('--android-sal','${vals[0]}');" +
            "s.setProperty('--android-sat','${vals[1]}');" +
            "s.setProperty('--android-sar','${vals[2]}');" +
            "s.setProperty('--android-sab','${vals[3]}');})();"
        webView.post { webView.evaluateJavascript(js, null) }
    }

    private fun ensurePlayerContainer() {
        val webView = mWebViewRef
        val content = findViewById<ViewGroup>(android.R.id.content)
        val parent = content ?: (webView?.parent as? ViewGroup) ?: return

        val existing = playerContainer
        if (existing != null) {
            if (existing.parent !== parent) {
                (existing.parent as? ViewGroup)?.removeView(existing)
                parent.addView(
                    existing,
                    0,
                    ViewGroup.LayoutParams(
                        ViewGroup.LayoutParams.MATCH_PARENT,
                        ViewGroup.LayoutParams.MATCH_PARENT
                    )
                )
            }
            return
        }

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
        container.addView(
            aspect,
            FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT,
                Gravity.CENTER
            )
        )
        container.addView(
            subs,
            FrameLayout.LayoutParams(
                ViewGroup.LayoutParams.MATCH_PARENT,
                ViewGroup.LayoutParams.MATCH_PARENT
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

    private fun triggerNativeFailure(exo: ExoPlayer) {
        hasError = true
        isBuffering = false
        isPaused = true
        playerContainer?.keepScreenOn = false
        exo.stop()
    }

    private fun updateKeepScreenOn(exo: ExoPlayer) {
        val state = exo.playbackState
        playerContainer?.keepScreenOn = exo.playWhenReady &&
            (state == Player.STATE_READY || state == Player.STATE_BUFFERING)
    }

    private fun ensurePlayer(): ExoPlayer {
        player?.let { return it }

        val renderersFactory = DefaultRenderersFactory(this)
            .setExtensionRendererMode(DefaultRenderersFactory.EXTENSION_RENDERER_MODE_ON)
            .setEnableDecoderFallback(true)
        val loadControl = DefaultLoadControl.Builder()
            .setBufferDurationsMs(2_500, 30_000, 1_000, 2_000)
            .build()
        val httpFactory = DefaultHttpDataSource.Factory()
            .setAllowCrossProtocolRedirects(false)
            .setConnectTimeoutMs(15_000)
            .setReadTimeoutMs(30_000)
        val audioAttrs = AudioAttributes.Builder()
            .setUsage(C.USAGE_MEDIA)
            .setContentType(C.AUDIO_CONTENT_TYPE_MOVIE)
            .build()
        val exo = ExoPlayer.Builder(this, renderersFactory)
            .setAudioAttributes(audioAttrs, true)
            .setHandleAudioBecomingNoisy(true)
            .setMediaSourceFactory(DefaultMediaSourceFactory(httpFactory))
            .setLoadControl(loadControl)
            .build()

        exo.addListener(object : Player.Listener {
            override fun onPlaybackStateChanged(playbackState: Int) {
                when (playbackState) {
                    Player.STATE_BUFFERING -> {
                        isBuffering = true
                        playbackStartedAtMs = 0L
                        updateKeepScreenOn(exo)
                    }
                    Player.STATE_READY -> {
                        val videoGroups = exo.currentTracks.groups.filter { it.type == C.TRACK_TYPE_VIDEO }
                        if (videoGroups.isNotEmpty() && !exo.currentTracks.isTypeSupported(C.TRACK_TYPE_VIDEO, true)) {
                            Log.w(TAG, "Video track unsupported at READY; falling back to transcode")
                            triggerNativeFailure(exo)
                            return
                        }
                        isBuffering = false
                        isReady = true
                        isEnded = false
                        if (exo.playWhenReady && !firstFrameRendered) {
                            playbackStartedAtMs = SystemClock.elapsedRealtime()
                        }
                        updateKeepScreenOn(exo)
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
                        playbackStartedAtMs = 0L
                        playerContainer?.keepScreenOn = false
                    }
                }
            }

            override fun onRenderedFirstFrame() {
                firstFrameRendered = true
                Log.i(TAG, "First video frame rendered")
            }

            override fun onIsPlayingChanged(isPlaying: Boolean) {
                updateKeepScreenOn(exo)
                if (isPlaying && !firstFrameRendered && playbackStartedAtMs == 0L) {
                    playbackStartedAtMs = SystemClock.elapsedRealtime()
                }
            }

            override fun onPlayWhenReadyChanged(playWhenReady: Boolean, reason: Int) {
                isPaused = !playWhenReady
                updateKeepScreenOn(exo)
                if (!playWhenReady) {
                    playbackStartedAtMs = 0L
                } else if (isReady && !firstFrameRendered && playbackStartedAtMs == 0L) {
                    playbackStartedAtMs = SystemClock.elapsedRealtime()
                }
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
                    videoRatio = ratio
                    Log.i(TAG, "Video size ${videoSize.width}x${videoSize.height} ratio=$ratio")
                    aspectLayout?.setAspectRatio(ratio)
                    applyScaleMode()
                }
            }

            override fun onCues(cueGroup: CueGroup) {
                subtitleView?.setCues(cueGroup.cues)
            }

            override fun onTracksChanged(tracks: Tracks) {
                val videoGroups = tracks.groups.filter { it.type == C.TRACK_TYPE_VIDEO }
                hasVideoTrack = videoGroups.isNotEmpty()
                if (hasVideoTrack && !tracks.isTypeSupported(C.TRACK_TYPE_VIDEO, true)) {
                    Log.w(TAG, "Unsupported video codec (e.g. MPEG-2/DivX); failing over to transcode")
                    triggerNativeFailure(exo)
                    return
                }
                applyTrackSelections(exo)
            }

            override fun onPlayerError(error: PlaybackException) {
                Log.e(TAG, "ExoPlayer error: ${error.errorCodeName}: ${error.message}", error)
                isBuffering = false
                hasError = true
                isPaused = true
                playerContainer?.keepScreenOn = false
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
        cachedEffectiveScale = computeEffectiveScaleMode()
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
        subtitleView?.setCues(emptyList())
        if (pendingSubIdx < 0) {
            builder.setTrackTypeDisabled(C.TRACK_TYPE_TEXT, true)
            builder.clearOverridesOfType(C.TRACK_TYPE_TEXT)
        } else if (pendingSubIdx in flatText.indices) {
            val (g, idx) = flatText[pendingSubIdx]
            builder.setTrackTypeDisabled(C.TRACK_TYPE_TEXT, false)
            builder.setOverrideForType(TrackSelectionOverride(g.mediaTrackGroup, idx))
        } else {
            builder.setTrackTypeDisabled(C.TRACK_TYPE_TEXT, true)
            builder.clearOverridesOfType(C.TRACK_TYPE_TEXT)
        }
        exo.trackSelectionParameters = builder.build()
    }

    private fun applyScaleMode() {
        val mode = when (videoScaleMode) {
            1 -> AspectRatioFrameLayout.RESIZE_MODE_FIXED_WIDTH
            2 -> AspectRatioFrameLayout.RESIZE_MODE_FIXED_HEIGHT
            else -> AspectRatioFrameLayout.RESIZE_MODE_FIT
        }
        aspectLayout?.let { aspect ->
            if (aspect.resizeMode != mode) {
                aspect.resizeMode = mode
            }
            aspect.requestLayout()
        }
        cachedEffectiveScale = computeEffectiveScaleMode()
    }

    private fun computeEffectiveScaleMode(): Int {
        if (videoScaleMode == 1 || videoScaleMode == 2) return videoScaleMode
        val w = playerContainer?.width ?: 0
        val h = playerContainer?.height ?: 0
        val vr = videoRatio
        return if (vr > 0f && w > 0 && h > 0) {
            val viewRatio = w.toFloat() / h.toFloat()
            if (vr >= viewRatio) 1 else 2
        } else {
            1
        }
    }

    fun kinoNativeOpen(
        seq: Long,
        url: String,
        startSec: Double,
        rate: Float,
        audioIdx: Int,
        subIdx: Int,
        autoplay: Boolean,
        scaleMode: Int
    ) {
        seekSeq = 0L
        currentPosSec = startSec.coerceAtLeast(0.0)
        durationSec = Double.NaN
        isPaused = !autoplay
        isBuffering = true
        isReady = false
        isEnded = false
        hasError = false
        hasVideoTrack = false
        firstFrameRendered = false
        playbackStartedAtMs = 0L
        videoScaleMode = scaleMode
        videoRatio = 0f
        openSeq = seq

        runOnUiThread {
            if (seq != openSeq) return@runOnUiThread
            ensurePlayerContainer()
            aspectLayout?.setAspectRatio(0f)
            applyScaleMode()
            mWebViewRef?.setBackgroundColor(Color.TRANSPARENT)
            mWebViewRef?.setLayerType(View.LAYER_TYPE_HARDWARE, null)
            playerContainer?.visibility = View.VISIBLE
            playerContainer?.requestLayout()
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
                if (exo.playbackState == Player.STATE_ENDED) {
                    exo.seekTo(0L)
                    isEnded = false
                }
                exo.playWhenReady = true
                exo.play()
                isPaused = false
                if (isReady && !firstFrameRendered && playbackStartedAtMs == 0L) {
                    playbackStartedAtMs = SystemClock.elapsedRealtime()
                }
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

    fun kinoNativeSetScale(mode: Int) {
        videoScaleMode = mode
        runOnUiThread {
            applyScaleMode()
        }
    }

    fun kinoNativeClose(seq: Long) {
        seekSeq = 0L
        isPaused = true
        isBuffering = false
        isReady = false
        isEnded = false
        hasError = false
        hasVideoTrack = false
        firstFrameRendered = false
        playbackStartedAtMs = 0L
        openSeq = seq
        runOnUiThread {
            tickerRunning = false
            mainHandler.removeCallbacks(tickRunnable)
            player?.release()
            player = null
            subtitleView?.setCues(emptyList())
            playerContainer?.keepScreenOn = false
            playerContainer?.visibility = View.GONE
            mWebViewRef?.setBackgroundColor(Color.TRANSPARENT)
        }
    }

    fun kinoNativePoll(): String {
        val b = { v: Boolean -> if (v) 1 else 0 }
        return "$openSeq|$seekSeq|$currentPosSec|$durationSec|${b(isPaused)}|${b(isBuffering)}|${b(isReady)}|${b(isEnded)}|${b(hasError)}|$cachedEffectiveScale"
    }
}
