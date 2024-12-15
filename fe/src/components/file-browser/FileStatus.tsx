import { FileEntry } from '@/types';
import { AlertTriangle, Check, Cloud, Timer } from 'lucide-react';

interface FileStatusProps {
    status: FileEntry['status'];
}

export function FileStatus({ status }: FileStatusProps) {
    const StatusIcon = {
        ready: () => <Check className="h-4 w-4 text-green-500" />,
        uploaded: () => <Cloud className="h-4 w-4 text-blue-500" />,
        uploading: () => <Timer className="h-4 w-4 text-yellow-500" />,
        unencrypted: () => <AlertTriangle className="h-4 w-4 text-red-500" />,
    }[status];

    return (
        <div className="flex items-center space-x-2">
            <StatusIcon />
            <span className="capitalize">{status}</span>
        </div>
    );
}
