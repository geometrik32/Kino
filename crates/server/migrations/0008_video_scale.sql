-- Remembered player picture scaling mode per title ('width' or 'height').
ALTER TABLE title_state
    ADD COLUMN video_scale TEXT CHECK (video_scale IN ('width', 'height'));
