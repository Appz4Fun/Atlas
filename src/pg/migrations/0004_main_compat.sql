-- A null repair cursor starts the next sweep from the newest release.
alter table repair_older alter column before drop not null;

-- Older SQLite repair marks can have no recorded completeness.
alter table repair_tried alter column permille drop not null;
