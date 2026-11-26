// The page entry: mount the app into the shell jinn serves.
import { mount } from './App';
import './style.css';

const parent = document.getElementById('app');
if (parent === null) throw new Error('the page shell has no #app element');
mount(parent);
