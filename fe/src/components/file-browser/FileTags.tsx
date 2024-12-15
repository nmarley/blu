import { Badge } from '@/components/ui/badge';

interface FileTagsProps {
    tags: string[];
}

export function FileTags({ tags }: FileTagsProps) {
    if (tags.length === 0) {
        return <span className="text-sm text-muted-foreground">No tags</span>;
    }

    return (
        <div className="flex flex-wrap gap-1">
            {tags.map((tag) => (
                <Badge key={tag} variant="secondary">
                    {tag}
                </Badge>
            ))}
        </div>
    );
}
