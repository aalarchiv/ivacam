import { describe, it, expect } from 'vitest';
import { isProjectPath } from './file-kind';

describe('isProjectPath', () => {
  it('treats JSON files as projects', () => {
    expect(isProjectPath('part.ivac-project.json')).toBe(true);
    expect(isProjectPath('legacy.vc-project.json')).toBe(true);
    expect(isProjectPath('whatever.json')).toBe(true);
    expect(isProjectPath('/abs/path/My Job.ivac-project.json')).toBe(true);
    expect(isProjectPath('C:\\Users\\me\\job.JSON')).toBe(true); // case-insensitive
  });

  it('treats drawings as not-projects', () => {
    expect(isProjectPath('drawing.dxf')).toBe(false);
    expect(isProjectPath('logo.svg')).toBe(false);
    expect(isProjectPath('/abs/path/part.DXF')).toBe(false);
    expect(isProjectPath('noextension')).toBe(false);
  });
});
