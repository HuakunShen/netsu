CREATE TABLE `entries` (
	`code` text PRIMARY KEY NOT NULL,
	`value` text NOT NULL,
	`created_at` integer NOT NULL,
	`expires_at` integer NOT NULL,
	`max_reads` integer NOT NULL,
	`remaining_reads` integer NOT NULL,
	`last_claim_id` text,
	CONSTRAINT "max_reads_range" CHECK("entries"."max_reads" BETWEEN 1 AND 100),
	CONSTRAINT "remaining_reads_non_negative" CHECK("entries"."remaining_reads" >= 0)
);
--> statement-breakpoint
CREATE INDEX `idx_entries_cleanup` ON `entries` (`expires_at`,`remaining_reads`);