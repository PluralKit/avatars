create table if not exists images
(
    id                     text primary key,
    url                    text        not null,
    original_url           text,
    original_file_size     int,
    original_type          text,
    original_attachment_id bigint,
    file_size              int         not null,
    width                  int         not null,
    height                 int         not null,
    kind                   text        not null,
    uploaded_at            timestamptz not null,
    uploaded_by_account    bigint
);

create index on images (original_url);
create index on images (original_attachment_id);
create index on images (uploaded_by_account);